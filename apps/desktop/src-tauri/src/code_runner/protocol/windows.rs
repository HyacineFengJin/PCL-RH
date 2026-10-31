//! Each pipe has its own bounded worker. The caller never cancels unrelated
//! thread IO or joins a pending synchronous call. Retirement retains the exact
//! worker, File, and buffers until repeated cancellation observes completion.
use super::super::Failure;
use std::{
    cell::RefCell,
    fs::File,
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE},
    System::{
        Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
        IO::CancelSynchronousIo,
    },
};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Slot {
    fn new() -> io::Result<Arc<Self>> {
        ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 16).then_some(count + 1)
            })
            .map_err(|_| io::Error::other("Build pipe retirement capacity is exhausted"))?;
        Ok(Arc::new(Self))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
fn retire_channel() -> io::Result<Sender<JoinHandle<()>>> {
    static POOL: OnceLock<Mutex<Option<Sender<JoinHandle<()>>>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| io::Error::other("Build pipe retirement is unavailable"))?;
    if let Some(sender) = &*pool {
        return Ok(sender.clone());
    }
    let (sender, receiver) = mpsc::channel::<JoinHandle<()>>();
    std::thread::Builder::new()
        .name("pcl-build-io-retire".into())
        .spawn(move || {
            let mut retired: Vec<JoinHandle<()>> = Vec::new();
            loop {
                match receiver.recv_timeout(Duration::from_millis(10)) {
                    Ok(worker) => retired.push(worker),
                    Err(mpsc::RecvTimeoutError::Disconnected) if retired.is_empty() => break,
                    _ => {}
                }
                retired.extend(receiver.try_iter());
                let mut index = 0;
                while index < retired.len() {
                    if retired[index].is_finished() {
                        let _ = retired.swap_remove(index).join();
                    } else {
                        unsafe {
                            CancelSynchronousIo(retired[index].as_raw_handle());
                        }
                        index += 1;
                    }
                }
            }
        })?;
    *pool = Some(sender.clone());
    Ok(sender)
}
enum Operation {
    Read,
    Write(Vec<u8>),
}
struct Call {
    operation: Operation,
    reply: SyncSender<io::Result<Vec<u8>>>,
}
struct Io {
    calls: Option<SyncSender<Call>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    retire: Sender<JoinHandle<()>>,
    _slot: Arc<Slot>,
    closed: Arc<AtomicBool>,
}
fn stopped(stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(io::Error::other("Build pipe is retired"))
    } else {
        Ok(())
    }
}
impl Io {
    fn new(mut file: File) -> io::Result<Self> {
        if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_PIPE {
            return Err(io::Error::other("Build transport requires anonymous pipes"));
        }
        let slot = Slot::new()?;
        let retire = retire_channel()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_slot = slot.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (calls, receiver) = mpsc::sync_channel::<Call>(1);
        let worker = std::thread::Builder::new()
            .name("pcl-build-io".into())
            .spawn(move || {
                let _slot = worker_slot;
                while let Ok(call) = receiver.recv() {
                    let result = (|| -> io::Result<Vec<u8>> {
                        stopped(&worker_stop)?;
                        match call.operation {
                            Operation::Read => {
                                let mut bytes = vec![0; 4096];
                                let length = match file.read(&mut bytes) {
                                    Ok(length) => length,
                                    Err(error)
                                        if matches!(error.raw_os_error(), Some(109 | 232)) =>
                                    {
                                        0
                                    }
                                    Err(error) => return Err(error),
                                };
                                bytes.truncate(length);
                                Ok(bytes)
                            }
                            Operation::Write(bytes) => {
                                let mut offset = 0;
                                while offset < bytes.len() {
                                    stopped(&worker_stop)?;
                                    let length = file.write(&bytes[offset..])?;
                                    if length == 0 {
                                        return Err(io::ErrorKind::WriteZero.into());
                                    }
                                    offset += length;
                                }
                                Ok(Vec::new())
                            }
                        }
                    })();
                    let failed = result.is_err();
                    let _ = call.reply.send(result);
                    if failed || worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                }
                drop(file);
                worker_closed.store(true, Ordering::Release);
            })?;
        Ok(Self {
            calls: Some(calls),
            worker: Some(worker),
            stop,
            retire,
            _slot: slot,
            closed,
        })
    }
    fn begin(&mut self, operation: Operation) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
        stopped(&self.stop)?;
        let (reply, receiver) = mpsc::sync_channel(1);
        self.calls
            .as_ref()
            .ok_or_else(|| io::Error::other("Build pipe is retired"))?
            .try_send(Call { operation, reply })
            .map_err(|_| io::Error::other("Build pipe already has an in-flight call"))?;
        Ok(receiver)
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            unsafe {
                CancelSynchronousIo(worker.as_raw_handle());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_runner::portable_tests::{io_fixture, pipe};

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "exact build pipe worker did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn pending_read_and_write_retire_without_joining_or_freeing_live_buffers() {
        let _guard = io_fixture();
        let (reader, _owner) = pipe();
        let mut read = Io::new(reader).unwrap();
        let _reply = read.begin(Operation::Read).unwrap();
        let closed = read.closed.clone();
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        drop(read);
        assert!(start.elapsed() < Duration::from_millis(250));
        wait_for(|| closed.load(Ordering::Acquire));

        let (_reader, writer) = pipe();
        let mut write = Io::new(writer).unwrap();
        let _reply = write
            .begin(Operation::Write(vec![b'x'; 64 * 1024]))
            .unwrap();
        let closed = write.closed.clone();
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        drop(write);
        assert!(start.elapsed() < Duration::from_millis(250));
        wait_for(|| closed.load(Ordering::Acquire));
    }

    #[test]
    fn sixteen_pending_workers_exhaust_admission_until_actual_retirement() {
        let _guard = io_fixture();
        wait_for(|| ACTIVE.load(Ordering::Acquire) == 0);
        let mut owners = Vec::new();
        let mut workers = Vec::new();
        let mut closed = Vec::new();
        for _ in 0..16 {
            let (reader, owner) = pipe();
            let mut io = Io::new(reader).unwrap();
            let _reply = io.begin(Operation::Read).unwrap();
            closed.push(io.closed.clone());
            owners.push(owner);
            workers.push(io);
        }
        let (reader, _owner) = pipe();
        assert!(Io::new(reader).is_err());
        assert_eq!(ACTIVE.load(Ordering::Acquire), 16);
        drop(workers);
        // Owner write ends remain alive, so successful retirement cannot be
        // attributed to EOF from the fixture closing the opposite pipe ends.
        wait_for(|| closed.iter().all(|closed| closed.load(Ordering::Acquire)));
        wait_for(|| ACTIVE.load(Ordering::Acquire) == 0);
        assert_eq!(owners.len(), 16);
    }
}
impl Drop for Io {
    fn drop(&mut self) {
        self.stop();
        drop(self.calls.take());
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                let _ = self.retire.send(worker);
            }
        }
    }
}
fn stdio(which: u32) -> io::Result<File> {
    let handle = unsafe { GetStdHandle(which) };
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if input == output {
        return Err(io::Error::other(
            "Build helper requires distinct pipe handles",
        ));
    }
    if handle.is_null()
        || handle == INVALID_HANDLE_VALUE
        || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE
    {
        return Err(io::Error::other("Build helper requires pipe stdio"));
    }
    // This early CLI branch alone consumes its inherited standard handles.
    Ok(unsafe { File::from_raw_handle(handle) })
}
pub(super) struct Reader {
    io: Io,
    pending: Option<Receiver<io::Result<Vec<u8>>>>,
}
impl Reader {
    pub(super) fn new(file: File) -> Result<Self, Failure> {
        Ok(Self {
            io: Io::new(file).map_err(|e| Failure::io("Cannot open build input pipe", e))?,
            pending: None,
        })
    }
    pub(super) fn closed(&self) -> Arc<AtomicBool> {
        self.io.closed.clone()
    }
    pub(super) fn stdin() -> Result<Self, Failure> {
        Self::new(stdio(STD_INPUT_HANDLE).map_err(|e| Failure::io("Cannot open build stdin", e))?)
    }
    pub(super) fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_none() {
            self.pending = Some(self.io.begin(Operation::Read)?);
        }
        match self.pending.as_ref().unwrap().try_recv() {
            Ok(result) => {
                self.pending = None;
                let result = result?;
                if result.len() > bytes.len() {
                    return Err(io::Error::other("Build input buffer is too small"));
                }
                bytes[..result.len()].copy_from_slice(&result);
                Ok(result.len())
            }
            Err(mpsc::TryRecvError::Empty) => Err(io::ErrorKind::WouldBlock.into()),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("Build input worker closed"))
            }
        }
    }
}
pub(super) struct Writer(RefCell<Io>);
impl Writer {
    pub(super) fn new(file: File) -> Result<Self, Failure> {
        Io::new(file)
            .map(|io| Self(RefCell::new(io)))
            .map_err(|e| Failure::io("Cannot open build output pipe", e))
    }
    pub(super) fn closed(&self) -> Arc<AtomicBool> {
        self.0.borrow().closed.clone()
    }
    pub(super) fn stdout() -> Result<Self, Failure> {
        Self::new(stdio(STD_OUTPUT_HANDLE).map_err(|e| Failure::io("Cannot open build stdout", e))?)
    }
    pub(super) fn write(&self, bytes: &[u8], deadline: Instant) -> Result<(), Failure> {
        if bytes.len() > super::MAX_EVENT + 1 || Instant::now() >= deadline {
            return Err(Failure::new(
                "BUILD_PROTOCOL_LIMIT",
                "Build output call exceeds its bound",
            ));
        }
        let mut io = self.0.borrow_mut();
        let receiver = io
            .begin(Operation::Write(bytes.to_vec()))
            .map_err(|e| Failure::io("Cannot submit build event", e))?;
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result
                .map(|_| ())
                .map_err(|e| Failure::io("Build event owner disconnected", e)),
            Err(_) => {
                io.stop();
                Err(Failure::new(
                    "BUILD_EVENT_TIMEOUT",
                    "Build event owner stopped reading",
                ))
            }
        }
    }
}

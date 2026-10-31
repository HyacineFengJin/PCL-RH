//! Windows bounded bootstrap IO. Synchronous anonymous-pipe calls run on a
//! private worker owning every File and buffer. Timeout never joins a blocked
//! call or frees its storage; one retirement worker repeats cancellation until
//! that exact IO thread has returned. No other application thread is cancelled.
use super::{FAILURE, REQUEST_LIMIT};
use pcl_core::game_process::helper_transport;
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Sender, SyncSender},
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
pub(super) type Child = helper_transport::OwnedHelper;
type Result<T> = std::result::Result<T, String>;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Slot {
    fn new() -> Result<Arc<Self>> {
        ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 16).then_some(n + 1)
            })
            .map_err(|_| "游戏监控传输仍在退役，请稍后重试")?;
        Ok(Arc::new(Self))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
fn retire_channel() -> Result<Sender<JoinHandle<()>>> {
    static POOL: OnceLock<Mutex<Option<Sender<JoinHandle<()>>>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| FAILURE)?;
    if let Some(sender) = pool.as_ref() {
        return Ok(sender.clone());
    }
    let (sender, receiver) = mpsc::channel::<JoinHandle<()>>();
    std::thread::Builder::new()
        .name("pcl-monitor-io-retire".into())
        .spawn(move || {
            let mut retired: Vec<JoinHandle<()>> = Vec::new();
            loop {
                match receiver.recv_timeout(Duration::from_millis(10)) {
                    Ok(worker) => retired.push(worker),
                    Err(mpsc::RecvTimeoutError::Disconnected) if retired.is_empty() => break,
                    _ => {}
                }
                for worker in receiver.try_iter() {
                    retired.push(worker);
                }
                let mut index = 0;
                while index < retired.len() {
                    if retired[index].is_finished() {
                        let worker = retired.swap_remove(index);
                        let _ = worker.join();
                    } else {
                        unsafe {
                            CancelSynchronousIo(retired[index].as_raw_handle());
                        }
                        index += 1;
                    }
                }
            }
        })
        .map_err(|_| FAILURE)?;
    *pool = Some(sender.clone());
    Ok(sender)
}
enum Operation {
    Write(Vec<u8>),
    Exact(usize),
    Line,
}
struct Call {
    operation: Operation,
    reply: SyncSender<Result<Vec<u8>>>,
}
pub(super) struct Io {
    calls: Option<Sender<Call>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    retire: Sender<JoinHandle<()>>,
    _slot: Arc<Slot>,
}
fn cancelled(stop: &AtomicBool) -> std::io::Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(std::io::Error::other(FAILURE))
    } else {
        Ok(())
    }
}
impl Io {
    fn new(mut input: File, mut output: File) -> Result<Self> {
        let slot = Slot::new()?;
        let retire = retire_channel()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_slot = slot.clone();
        let (calls, receiver) = mpsc::channel::<Call>();
        let worker = std::thread::Builder::new()
            .name("pcl-monitor-io".into())
            .spawn(move || {
                let _slot = worker_slot;
                while let Ok(call) = receiver.recv() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let result = (|| -> std::io::Result<Vec<u8>> {
                        match call.operation {
                            Operation::Write(mut bytes) => {
                                let result = (|| {
                                    let mut offset = 0;
                                    while offset < bytes.len() {
                                        cancelled(&worker_stop)?;
                                        let count = output.write(&bytes[offset..])?;
                                        if count == 0 {
                                            return Err(std::io::Error::other(FAILURE));
                                        }
                                        offset += count;
                                    }
                                    Ok(Vec::new())
                                })();
                                bytes.fill(0);
                                result
                            }
                            Operation::Exact(length) => {
                                let mut bytes = vec![0; length];
                                let mut offset = 0;
                                while offset < length {
                                    cancelled(&worker_stop)?;
                                    let count = input.read(&mut bytes[offset..])?;
                                    if count == 0 {
                                        return Err(std::io::Error::other(FAILURE));
                                    }
                                    offset += count;
                                }
                                Ok(bytes)
                            }
                            Operation::Line => {
                                let mut bytes = Vec::new();
                                loop {
                                    cancelled(&worker_stop)?;
                                    let mut byte = [0];
                                    input.read_exact(&mut byte)?;
                                    if byte[0] == b'\n' {
                                        break;
                                    }
                                    bytes.push(byte[0]);
                                    if bytes.len() > 128 {
                                        return Err(std::io::Error::other(FAILURE));
                                    }
                                }
                                Ok(bytes)
                            }
                        }
                    })()
                    .map_err(|_| FAILURE.to_owned());
                    let failed = result.is_err();
                    let _ = call.reply.send(result);
                    if failed {
                        break;
                    }
                }
            })
            .map_err(|_| FAILURE)?;
        Ok(Self {
            calls: Some(calls),
            worker: Some(worker),
            stop,
            retire,
            _slot: slot,
        })
    }
    fn call(&mut self, operation: Operation, deadline: Instant) -> Result<Vec<u8>> {
        if self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(FAILURE.into());
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        self.calls
            .as_ref()
            .ok_or(FAILURE)?
            .send(Call { operation, reply })
            .map_err(|_| FAILURE)?;
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(_) => {
                self.stop.store(true, Ordering::Release);
                if let Some(worker) = &self.worker {
                    unsafe {
                        CancelSynchronousIo(worker.as_raw_handle());
                    }
                }
                Err(FAILURE.into())
            }
        }
    }
    pub(super) fn write(&mut self, bytes: &[u8], deadline: Instant) -> Result<()> {
        if bytes.len() > REQUEST_LIMIT {
            return Err(FAILURE.into());
        }
        self.call(Operation::Write(bytes.to_vec()), deadline)
            .map(|_| ())
    }
    pub(super) fn read_exact(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<()> {
        if bytes.len() > REQUEST_LIMIT {
            return Err(FAILURE.into());
        }
        let mut read = self.call(Operation::Exact(bytes.len()), deadline)?;
        bytes.copy_from_slice(&read);
        read.fill(0);
        Ok(())
    }
    pub(super) fn read_line(&mut self, deadline: Instant) -> Result<String> {
        String::from_utf8(self.call(Operation::Line, deadline)?).map_err(|_| FAILURE.into())
    }
    pub(super) fn helper() -> Result<Self> {
        let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        for handle in [input, output] {
            if handle.is_null()
                || handle == INVALID_HANDLE_VALUE
                || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE
            {
                return Err(FAILURE.into());
            }
        }
        if input == output {
            return Err(FAILURE.into());
        }
        // Only this early CLI mode consumes its inherited stdio. The worker is
        // their sole owner and closes them after ACK or any bootstrap failure.
        unsafe { Self::new(File::from_raw_handle(input), File::from_raw_handle(output)) }
    }
}
impl Drop for Io {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.calls.take());
        if let Some(worker) = self.worker.take() {
            unsafe {
                CancelSynchronousIo(worker.as_raw_handle());
            }
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                let _ = self.retire.send(worker);
            }
        }
    }
}
pub(super) fn spawn(executable: &Path) -> Result<(Child, Io)> {
    let helper_transport::Pipes {
        mut child,
        input,
        output,
    } = helper_transport::spawn(executable)?;
    match Io::new(output, input) {
        Ok(io) => Ok((child, io)),
        Err(error) => {
            child.kill_owned();
            Err(error)
        }
    }
}
pub(super) fn kill_owned(child: &mut Child) {
    child.kill_owned();
}

//! Real loopback HTTP through the actual chooser authority, transfer writer,
//! readback seal and commit gate. Every filesystem entry belongs to a TempDir.
use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::AtomicUsize,
    thread,
    time::{Duration, Instant},
};
fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Runtime::new().unwrap().block_on(future)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    chosen: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = Directory::open_absolute(&base).unwrap();
        drop(
            root.create_private(&Leaf::new("chosen").unwrap(), Traversal::AllowMounts)
                .unwrap(),
        );
        let mut decoy = root.create_file(&Leaf::new("decoy").unwrap()).unwrap();
        decoy.write_all(b"kept original").unwrap();
        Self {
            chosen: base.join("chosen"),
            base,
            _temp: temp,
        }
    }
    fn pending(&self, url: &str) -> PreparedDownload {
        self.pending_name(url, "sample.bin")
    }
    fn pending_name(&self, url: &str, name: &str) -> PreparedDownload {
        let session = DownloadSession::default();
        let (generation, _) = session.begin_choose();
        let directory = session
            .adopt_directory(generation, self.chosen.clone())
            .unwrap();
        let preview = session
            .prepare(
                PrepareRequest {
                    directory_token: directory.token,
                    url: url.into(),
                    file_name: name.into(),
                },
                "fixture-prefs".into(),
            )
            .unwrap();
        let pending = session
            .start_guard(&preview.token, "fixture-prefs")
            .unwrap()
            .take();
        pending
    }
    fn target(&self) -> PathBuf {
        self.chosen.join("sample.bin")
    }
    fn assert_empty(&self) {
        assert_eq!(fs::read_dir(&self.chosen).unwrap().count(), 0);
        self.decoy();
    }
    fn decoy(&self) {
        assert_eq!(fs::read(self.base.join("decoy")).unwrap(), b"kept original");
    }
}
struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(reply: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let signal = stop.clone();
        let seen = requests.clone();
        let reply = Arc::new(reply);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut workers = Vec::new();
            while !signal.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        if workers.len() >= 8 {
                            break;
                        }
                        // Accepted sockets can inherit nonblocking on BSD/Win.
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let seen = seen.clone();
                        let reply = reply.clone();
                        workers.push(thread::spawn(move || {
                            let mut request = Vec::new();
                            let mut buffer = [0u8; 1024];
                            while request.len() < 8192
                                && !request.windows(4).any(|window| window == b"\r\n\r\n")
                            {
                                let count = match socket.read(&mut buffer) {
                                    Ok(count) => count,
                                    Err(_) => return,
                                };
                                if count == 0 {
                                    return;
                                }
                                request.extend_from_slice(&buffer[..count]);
                            }
                            seen.fetch_add(1, Ordering::Relaxed);
                            let text = String::from_utf8_lossy(&request);
                            let path = text
                                .lines()
                                .next()
                                .unwrap_or("")
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or("");
                            let _ = socket.write_all(&reply(path));
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            stop,
            requests,
            worker: Some(worker),
        }
    }
    fn body(bytes: Vec<u8>) -> Self {
        Self::new(move |_| {
            let mut reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .into_bytes();
            reply.extend_from_slice(&bytes);
            reply
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn scheduler() -> Arc<DownloadScheduler> {
    DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: 2,
        ..Default::default()
    })
    .unwrap()
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .unwrap()
}

#[test]
fn native_toolbox_actual_download_keeps_typed_owner_until_commit_and_reports_actual_sha() {
    for bytes in [b"native HTTP bytes\0\xff".to_vec(), Vec::new()] {
        let fixture = Fixture::new();
        let server = Server::body(bytes.clone());
        let calls = AtomicUsize::new(0);
        let network = Arc::new(
            ClientFactory::new(pcl_network::Policy {
                proxy: pcl_network::ProxyPolicy::None,
                dns: pcl_network::DnsPolicy::System,
            })
            .unwrap(),
        );
        let mut gate = || {
            calls.fetch_add(1, Ordering::Relaxed);
            assert!(!fixture.target().exists());
            Ok(())
        };
        let result = run(download(
            fixture.pending(&server.url),
            network,
            scheduler(),
            &AtomicBool::new(false),
            |_| {},
            &mut gate,
        ))
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(result.size, bytes.len() as u64);
        assert_eq!(result.network_bytes, bytes.len() as u64);
        assert_eq!(result.sha256, hash(&bytes));
        assert!(result.warning.is_none(), "{:?}", result.warning);
        assert_eq!(fs::read(fixture.target()).unwrap(), bytes);
        assert_eq!(fs::read_dir(&fixture.chosen).unwrap().count(), 1);
        fixture.decoy();
    }
}

#[test]
fn native_toolbox_http_failures_and_size_limits_discard_only_owned_partial_bytes() {
    for response in [b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nsh".to_vec(), b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\ntoo-long".to_vec(), b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody".to_vec()] {
        let fixture = Fixture::new(); let server = Server::new(move |_| response.clone()); let mut gate = || panic!("failed HTTP cannot enter the commit gate");
        let limits = transfer::TransferLimits { maximum_bytes: 4, ..Default::default() };
        assert!(run(download_with_client(fixture.pending(&server.url), &client(), &scheduler(), &AtomicBool::new(false), |_| {}, &mut gate, limits)).is_err()); fixture.assert_empty();
    }
}

#[test]
fn native_toolbox_redirect_validates_hops_and_keeps_url_query_out_of_preview() {
    let fixture = Fixture::new();
    let server = Server::new(|path| {
        if path.starts_with("/first") {
            b"HTTP/1.1 302 Found\r\nLocation: /final?private=fixture-secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ngood".to_vec()
        }
    });
    let prepared = fixture.pending(&format!("{}/first?private=fixture-secret", server.url));
    assert!(!prepared.preview.url_display.contains("fixture-secret"));
    let mut gate = || Ok(());
    let result = run(download_with_client(
        prepared,
        &client(),
        &scheduler(),
        &AtomicBool::new(false),
        |_| {},
        &mut gate,
        transfer::TransferLimits::default(),
    ))
    .unwrap();
    assert_eq!(result.sha256, hash(b"good"));
    assert_eq!(server.requests.load(Ordering::Relaxed), 2);
    assert_eq!(fs::read(fixture.target()).unwrap(), b"good");
    assert!(transfer::redirect_url(
        &authority::validate_url("https://example.invalid/a").unwrap(),
        "http://example.invalid/b"
    )
    .is_err());
}

#[test]
fn native_toolbox_body_cancellation_and_commit_gate_failure_clean_typed_staging() {
    for at_gate in [false, true] {
        let fixture = Fixture::new();
        let server = Server::body(vec![b'x'; 300_000]);
        let cancel = AtomicBool::new(false);
        let mut gate = || Err("retired commit authority".into());
        let result = run(download_with_client(
            fixture.pending(&server.url),
            &client(),
            &scheduler(),
            &cancel,
            |progress| {
                if !at_gate && progress.phase == "downloading" && progress.bytes_done > 0 {
                    cancel.store(true, Ordering::Release);
                }
            },
            &mut gate,
            transfer::TransferLimits::default(),
        ));
        let error = result.err().unwrap();
        assert!(
            if at_gate {
                error.contains("retired commit authority")
            } else {
                error.contains(CANCELLED)
            },
            "{error}"
        );
        fixture.assert_empty();
    }
}

#[test]
fn native_toolbox_final_collision_and_last_cancel_keep_targets_unmodified() {
    for collision in [false, true] {
        let fixture = Fixture::new();
        let server = Server::body(b"good".to_vec());
        let cancel = AtomicBool::new(false);
        let mut gate = || {
            if collision {
                fs::write(fixture.target(), b"outside original").unwrap();
            } else {
                cancel.store(true, Ordering::Release);
            }
            Ok(())
        };
        assert!(run(download_with_client(
            fixture.pending(&server.url),
            &client(),
            &scheduler(),
            &cancel,
            |_| {},
            &mut gate,
            transfer::TransferLimits::default()
        ))
        .is_err());
        if collision {
            assert_eq!(fs::read(fixture.target()).unwrap(), b"outside original");
            assert_eq!(fs::read_dir(&fixture.chosen).unwrap().count(), 1);
        } else {
            fixture.assert_empty();
        }
        fixture.decoy();
    }
}

#[test]
fn native_toolbox_tokens_and_parent_replacement_are_bound_to_the_actual_directory() {
    let fixture = Fixture::new();
    let session = DownloadSession::default();
    let (generation, _) = session.begin_choose();
    let directory = session
        .adopt_directory(generation, fixture.chosen.clone())
        .unwrap();
    let request = PrepareRequest {
        directory_token: directory.token.clone(),
        url: "https://example.invalid/file".into(),
        file_name: "sample.bin".into(),
    };
    let first = session.prepare(request.clone(), "prefs".into()).unwrap();
    let second = session.prepare(request.clone(), "prefs".into()).unwrap();
    assert_ne!(first.token, second.token);
    assert!(session.start_guard(&first.token, "prefs").is_err());
    assert!(session.start_guard(&second.token, "other-prefs").is_err());
    if fs::rename(&fixture.chosen, fixture.base.join("retained-chosen")).is_ok() {
        fs::create_dir(&fixture.chosen).unwrap();
        assert!(session.held_directory(&directory.token).is_err());
        assert!(session.prepare(request, "prefs".into()).is_err());
    }
    fixture.decoy();
}

#[test]
fn native_toolbox_failed_sync_after_publication_is_a_warning_and_preserves_bytes() {
    let fixture = Fixture::new();
    let directory = files::Directory::open(&fixture.chosen).unwrap();
    let mut stage = directory.anonymous().unwrap();
    stage.write_all(b"good").unwrap();
    let verified =
        files::verify_anonymous(stage, 4, &hash(b"good"), &AtomicBool::new(false)).unwrap();
    let warning = directory
        .publish_with_failed_sync("sample.bin", &verified)
        .unwrap()
        .unwrap();
    assert!(warning.contains("文件已保存"), "{warning}");
    assert_eq!(fs::read(fixture.target()).unwrap(), b"good");
    drop(verified);
    assert_eq!(fs::read_dir(&fixture.chosen).unwrap().count(), 1);
    fixture.decoy();
}

#[test]
fn native_toolbox_unknown_staging_or_changed_verified_bytes_never_overwrite_a_target() {
    let fixture = Fixture::new();
    let directory = files::Directory::open(&fixture.chosen).unwrap();
    let mut stage = directory.anonymous().unwrap();
    stage.write_all(b"good").unwrap();
    let mut external = files::staging_file(&mut stage).try_clone().unwrap();
    let verified =
        files::verify_anonymous(stage, 4, &hash(b"good"), &AtomicBool::new(false)).unwrap();
    use std::io::{Seek, SeekFrom};
    external.seek(SeekFrom::Start(0)).unwrap();
    external.write_all(b"bad!").unwrap();
    external.sync_all().unwrap();
    drop(external);
    assert!(directory.publish("sample.bin", &verified).is_err());
    assert!(!fixture.target().exists());
    fixture.decoy();
    // An edited sealed native source remains evidence; no unknown workspace
    // content is deleted to turn this failure into success.
}

#[cfg(unix)]
#[test]
fn native_toolbox_symlink_chooser_and_existing_symlink_targets_are_refused() {
    let fixture = Fixture::new();
    let alias = fixture.base.join("alias");
    std::os::unix::fs::symlink(&fixture.chosen, &alias).unwrap();
    let session = DownloadSession::default();
    let (generation, _) = session.begin_choose();
    assert!(session.adopt_directory(generation, alias).is_err());
    let prepared = fixture.pending("https://example.invalid/file");
    std::os::unix::fs::symlink(fixture.base.join("decoy"), fixture.target()).unwrap();
    assert!(prepared.directory.recheck().is_ok());
    assert!(prepared.directory.directory.absent("sample.bin").is_err());
    fixture.decoy();
}

#[test]
fn native_toolbox_independent_files_share_a_directory_without_losing_stage_owners() {
    let fixture = Fixture::new();
    let server = Server::body(b"concurrent bytes".to_vec());
    let first = fixture.pending_name(&server.url, "first.bin");
    let second = fixture.pending_name(&server.url, "second.bin");
    let cancel = AtomicBool::new(false);
    let scheduler = scheduler();
    let client = client();
    let mut first_gate = || Ok(());
    let mut second_gate = || Ok(());
    let (one, two) = run(async {
        tokio::join!(
            download_with_client(
                first,
                &client,
                &scheduler,
                &cancel,
                |_| {},
                &mut first_gate,
                transfer::TransferLimits::default()
            ),
            download_with_client(
                second,
                &client,
                &scheduler,
                &cancel,
                |_| {},
                &mut second_gate,
                transfer::TransferLimits::default()
            )
        )
    });
    assert!(one.unwrap().warning.is_none());
    assert!(two.unwrap().warning.is_none());
    assert_eq!(
        fs::read(fixture.chosen.join("first.bin")).unwrap(),
        b"concurrent bytes"
    );
    assert_eq!(
        fs::read(fixture.chosen.join("second.bin")).unwrap(),
        b"concurrent bytes"
    );
    assert_eq!(fs::read_dir(&fixture.chosen).unwrap().count(), 2);
    fixture.decoy();
}

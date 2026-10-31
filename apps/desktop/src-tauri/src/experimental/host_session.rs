//! One real trusted Node host session. The UI and permission dispatchers share
//! this exact bounded transport; this module never evaluates author builds.
use super::{
    host_files::Lease,
    host_io::{self, Channel},
    host_process::{self, Lifecycle, Node, Process},
};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) struct Session {
    pub(super) child: Process,
    channel: Option<Channel>,
    pub(super) broken: bool,
}
impl Session {
    pub(super) fn start(project: &Path, lifecycle: Lifecycle) -> Result<Self, String> {
        let lease = Arc::new(
            Lease::open(project).map_err(|error| format!("无法取得实验功能宿主锁：{error}"))?,
        );
        let node = Node::discover().map_err(|error| {
            let message = error.to_string();
            if error.kind() == std::io::ErrorKind::NotFound
                && message.starts_with(host_process::NODE_MISSING)
            {
                message
            } else {
                format!("无法定位系统 Node.js 22.19+：{message}")
            }
        })?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut probe = Process::spawn(
            node.version_command().map_err(|e| e.to_string())?,
            lease.clone(),
            lifecycle.clone(),
        )
        .map_err(|error| format!("无法探测系统 Node.js 版本：{error}"))?;
        let checked = (|| {
            node.recheck()?;
            let bytes = host_io::version(&mut probe, deadline)?;
            let status = probe.wait_exit(deadline)?;
            check_version(status.success(), &bytes)?;
            node.recheck()?;
            lease.recheck()
        })();
        let completed = probe.completion();
        drop(probe);
        let retired =
            host_process::wait_retired(&completed, Instant::now() + Duration::from_millis(500));
        checked.map_err(|error| {
            let message = error.to_string();
            if error.kind() == std::io::ErrorKind::Unsupported
                && message.starts_with(host_process::NODE_UNSUPPORTED)
            {
                message
            } else {
                format!("无法使用系统 Node.js 22.19+：{message}")
            }
        })?;
        if !retired {
            return Err("Node.js 版本探测进程尚未完成退役，请稍后重试".into());
        }
        let command = node.host_command(&lease).map_err(|e| e.to_string())?;
        let child = Process::spawn(command, lease.clone(), lifecycle.clone())
            .map_err(|error| format!("无法启动实验功能引擎（需要 Node.js 22.19+）：{error}"))?;
        node.recheck()
            .and_then(|_| lease.recheck())
            .map_err(|e| e.to_string())?;
        let session = Self::from_process(child)?;
        lifecycle
            .stopped
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(session)
    }
    pub(super) fn from_process(mut child: Process) -> Result<Self, String> {
        let channel =
            Channel::open(&mut child).map_err(|error| format!("无法打开实验功能通道：{error}"))?;
        Ok(Self {
            child,
            channel: Some(channel),
            broken: false,
        })
    }
    pub(super) fn call(&mut self, operation: &str, args: Value) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let text = serde_json::to_vec(&json!({"operation":operation,"args":args}))
            .map_err(|e| e.to_string())?;
        let limit = match operation {
            "rh_code_run" => 24 * 1024 * 1024,
            "extensions_settings" => 512 * 1024,
            _ => 128_000,
        };
        if text.len() > limit {
            return Err(format!("引擎请求超过 {} KB", limit / 1000));
        }
        self.broken = true;
        let reply_limit = if operation == "rh_code_harness_export" {
            24 * 1024 * 1024
        } else {
            1_000_000
        };
        let line = self
            .channel
            .as_mut()
            .ok_or("实验功能引擎已断开")?
            .request(&text, reply_limit, deadline)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    "实验功能引擎未及时响应".to_string()
                } else {
                    format!("实验功能通道失败：{error}")
                }
            })?;
        let reply: Value =
            serde_json::from_slice(&line).map_err(|_| "实验功能引擎返回了无效结果".to_string())?;
        self.broken = false;
        if reply["ok"] == true {
            Ok(reply["result"].clone())
        } else {
            Err(reply["error"]
                .as_str()
                .unwrap_or("实验功能处理失败")
                .to_owned())
        }
    }
    /// Normal idle/exit paths request the existing shutdown operation under its
    /// bounded RPC deadline. Drop itself performs no RPC and no blocking wait.
    pub(super) fn retire(mut self, graceful: bool) -> bool {
        if graceful && !self.broken {
            let _ = self.call("shutdown", json!({}));
        }
        let completed = self.child.completion();
        drop(self.channel.take());
        drop(self);
        host_process::wait_retired(&completed, Instant::now() + Duration::from_millis(500))
    }
}

fn check_version(success: bool, bytes: &[u8]) -> std::io::Result<()> {
    if !success {
        return Err(std::io::Error::other("系统 Node.js 版本探测进程执行失败"));
    }
    if host_process::parse_version(bytes).is_none() {
        return Err(std::io::Error::other(
            "系统 Node.js 版本探测返回了无效版本信息",
        ));
    }
    if !host_process::supported_version(bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "{} 需要系统 Node.js 22.19 或更高版本",
                host_process::NODE_UNSUPPORTED
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_successfully_parsed_old_versions_are_unsupported() {
        for bytes in [b"v22.18.9\n".as_slice(), b"v20.1.0"] {
            let error = check_version(true, bytes).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
            assert!(error
                .to_string()
                .starts_with(host_process::NODE_UNSUPPORTED));
        }
        for (success, bytes) in [
            (false, b"v20.1.0".as_slice()),
            (false, b"v24.0.0"),
            (true, b"not a version"),
            (true, b"\xff"),
            (true, b"v22.19.+1"),
        ] {
            let error = check_version(success, bytes).unwrap_err();
            assert!(!error
                .to_string()
                .starts_with(host_process::NODE_UNSUPPORTED));
            assert!(!error.to_string().starts_with(host_process::NODE_MISSING));
        }
        assert!(check_version(true, b"v22.19.0\n").is_ok());
        assert!(check_version(true, b"v24.0.0\n").is_ok());
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Pipes are cancelled/closed before Process transfers its exact child
        // and host.lock to retirement; live Windows buffers stay on its worker.
        drop(self.channel.take());
    }
}

use super::*;
use reqwest::blocking::Client;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

fn with_response<T>(
    status: u16,
    body: Vec<u8>,
    length: Option<usize>,
    inspect: impl FnOnce(Response) -> T,
) -> T {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/response", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while request.len() < 8192 && stream.read_exact(&mut byte).is_ok() {
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let mut wire = format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n");
        if let Some(size) = length {
            wire.push_str(&format!("Content-Length: {size}\r\n"));
        }
        wire.push_str("\r\n");
        let _ = stream.write_all(wire.as_bytes());
        let _ = stream.write_all(&body);
    });
    let response = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
        .get(url)
        .send()
        .unwrap();
    // Consume the body while the writer is alive. Joining first depends on OS
    // socket buffer sizes and can truncate the oversize fixture on Windows.
    let result = inspect(response);
    worker.join().unwrap();
    result
}
fn decode_body(
    stage: AuthStage,
    status: u16,
    body: Vec<u8>,
    length: Option<usize>,
) -> Result<Value, String> {
    with_response(status, body, length, |response| decode(stage, Ok(response)))
}
fn decode_json(stage: AuthStage, status: u16, value: Value) -> Result<Value, String> {
    let bytes = serde_json::to_vec(&value).unwrap();
    let size = bytes.len();
    decode_body(stage, status, bytes, Some(size))
}

#[test]
fn successful_json_remains_unchanged() {
    let value = json!({"access_token":"synthetic-token", "expires_in":3600});
    assert_eq!(
        decode_json(AuthStage::MicrosoftToken, 200, value.clone()).unwrap(),
        value
    );
}
#[test]
fn html_outage_and_rate_limits_report_stage_and_http_status_without_body() {
    for status in [429, 500, 502, 503] {
        let body = b"<html>private-provider-detail</html>".to_vec();
        let size = body.len();
        let error = decode_body(AuthStage::XboxLive, status, body, Some(size)).unwrap_err();
        assert!(error.contains("Xbox Live"));
        assert!(error.contains(&format!("HTTP {status}")));
        assert!(!error.contains("private-provider-detail"));
        assert!(!error.contains("格式错误"));
    }
}
#[test]
fn forbidden_is_not_automatically_an_application_approval_failure() {
    let error = decode_json(
        AuthStage::MinecraftLogin,
        403,
        json!({"error_description":"private-provider-detail"}),
    )
    .unwrap_err();
    assert!(error.contains("Minecraft 登录"));
    assert!(error.contains("HTTP 403"));
    assert!(!error.contains("尚未获得"));
    assert!(!error.contains("private-provider-detail"));
}
#[test]
fn known_microsoft_and_xbox_errors_keep_their_actionable_messages() {
    let invalid = decode_json(
        AuthStage::MicrosoftToken,
        400,
        json!({"error":"invalid_grant", "error_description":"synthetic-secret"}),
    )
    .unwrap_err();
    assert_eq!(invalid, "微软登录凭据已失效，请重新登录");
    let family = decode_json(
        AuthStage::Xsts,
        401,
        json!({"XErr":2148916238u64,"Message":"synthetic-secret"}),
    )
    .unwrap_err();
    assert!(family.contains("家庭"));
    assert!(!family.contains("synthetic-secret"));
}
#[test]
fn pending_and_slow_down_json_remain_available_to_the_device_poller() {
    for code in ["authorization_pending", "slow_down"] {
        let body = serde_json::to_vec(&json!({"error":code})).unwrap();
        let size = body.len();
        with_response(400, body, Some(size), |response| {
            let response = receive(AuthStage::MicrosoftToken, Ok(response)).unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                read_json(AuthStage::MicrosoftToken, response).unwrap()["error"],
                code
            );
        });
    }
}
#[test]
fn invalid_success_json_and_error_html_are_distinguished() {
    for status in [200, 401] {
        let error = decode_body(
            AuthStage::Profile,
            status,
            b"private-html".to_vec(),
            Some(12),
        )
        .unwrap_err();
        assert!(error.contains("Minecraft 玩家档案"));
        assert!(!error.contains("private-html"));
        if status == 200 {
            assert!(error.contains("格式错误"));
        } else {
            assert!(error.contains("HTTP 401"));
        }
    }
}
#[test]
fn declared_and_streamed_oversize_responses_are_bounded() {
    for length in [Some(MAX_RESPONSE_BYTES + 1), None] {
        let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let error = decode_body(AuthStage::DeviceCode, 200, body, length).unwrap_err();
        assert!(error.contains("大小上限"));
        assert!(error.contains("微软设备登录码"));
    }
}
#[test]
fn interrupted_body_does_not_echo_transport_or_response_text() {
    let error = decode_body(
        AuthStage::Entitlements,
        200,
        b"private-body".to_vec(),
        Some(1024),
    )
    .unwrap_err();
    assert!(error.contains("Minecraft 游戏资格"));
    assert!(error.contains("读取认证响应失败"));
    assert!(!error.contains("private-body"));
    assert!(!error.contains("http://"));
}
#[test]
fn redirects_are_rejected_with_a_fixed_stage_message() {
    let error = decode_body(AuthStage::Xsts, 302, Vec::new(), Some(0)).unwrap_err();
    assert!(error.contains("XSTS"));
    assert!(error.contains("HTTP 302"));
}
#[test]
fn failed_connection_does_not_reveal_the_request_url() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/private-route?token=synthetic-secret",
        listener.local_addr().unwrap()
    );
    drop(listener);
    let request = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap()
        .get(url)
        .send();
    let error = decode(AuthStage::MicrosoftToken, request).unwrap_err();
    assert!(error.contains("微软登录凭据"));
    assert!(error.contains("无法建立连接"));
    assert!(!error.contains("synthetic-secret"));
    assert!(!error.contains("http://"));
}
#[test]
fn timeout_is_distinct_from_connection_failure_without_a_real_service() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/private-route?token=synthetic-secret",
        listener.local_addr().unwrap()
    );
    let (done, wait) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 8192];
        let _ = stream.read(&mut request);
        let _ = wait.recv_timeout(Duration::from_secs(2));
    });
    let request = Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(150))
        .build()
        .unwrap()
        .get(url)
        .send();
    let error = decode(AuthStage::XboxLive, request).unwrap_err();
    let _ = done.send(());
    worker.join().unwrap();
    assert!(error.contains("Xbox Live"));
    assert!(error.contains("超时"));
    assert!(!error.contains("synthetic-secret"));
    assert!(!error.contains("http://"));
}

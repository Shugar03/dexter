use dexter_browser::BrowserDriver;
use dexter_core::{ActionStatus, CredentialField, ObservationScope, SemanticTarget, Target};
use dexter_driver::{ActContext, ComputerDriver};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct HttpFixture {
    origin: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind login fixture");
        let port = listener.local_addr().unwrap().port();
        listener
            .set_nonblocking(true)
            .expect("set fixture listener nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        let body = r#"<!doctype html><html><body>
                            <form>
                              <input id="user" type="text" aria-label="Username">
                              <input id="pw" type="password" aria-label="Password">
                              <input id="note" type="text" aria-label="Note">
                            </form>
                          </body></html>"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("login fixture accept: {error}"),
                }
            }
        });
        Self {
            origin: format!("http://127.0.0.1:{port}"),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_chromedriver(child: &mut Child, port: u16) {
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll chromedriver") {
            panic!("chromedriver exited during startup: {status}");
        }
        if TcpStream::connect(&address).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("chromedriver did not listen on {address} within 10s");
}

fn target(name: &str) -> Target {
    Target::Semantic(SemanticTarget {
        role: Some("text_field".into()),
        name: Some(name.into()),
        ..Default::default()
    })
}

#[test]
#[ignore = "requires Chrome for Testing at ~/.local/bin and DISPLAY=:0"]
fn chrome_fills_only_origin_bound_password_fields_without_observation_leak() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let bin_dir = home.join(".local/bin");
    let chrome = bin_dir.join("google-chrome");
    let chromedriver = bin_dir.join("chromedriver");
    assert!(
        chrome.is_file(),
        "missing Chrome for Testing at {}",
        chrome.display()
    );
    assert!(
        chromedriver.is_file(),
        "missing chromedriver at {}",
        chromedriver.display()
    );

    let fixture = HttpFixture::start();
    let port = free_port();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let child = Command::new(chromedriver)
        .arg(format!("--port={port}"))
        .arg("--allowed-ips=127.0.0.1")
        .env("PATH", path)
        .env("DISPLAY", ":0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start chromedriver");
    let mut child = ChildGuard(child);
    wait_for_chromedriver(&mut child.0, port);

    let driver = BrowserDriver::connect(&format!("http://127.0.0.1:{port}"), "chrome")
        .expect("connect to chromedriver");
    driver.navigate(&fixture.origin).expect("navigate to form");
    let observation = driver
        .observe(&ObservationScope::default())
        .expect("observe login form");
    assert!(
        observation
            .elements
            .iter()
            .any(|element| element.name.as_deref() == Some("Password")),
        "login form password input was not observed"
    );

    let secret = "chrome-live-secret-623a";
    let password = target("Password");
    let success = driver
        .fill_secret(
            &password,
            CredentialField::Password,
            secret,
            &fixture.origin,
            &ActContext::default(),
        )
        .expect("fill password");
    assert_eq!(success.status, ActionStatus::Success);
    let stored = driver
        .client_exec("return document.querySelector('#pw').value;", vec![])
        .expect("read password value");
    assert_eq!(stored.as_str(), Some(secret));

    let wrong_port = free_port();
    let wrong_origin = format!("http://localhost:{wrong_port}");
    let mismatch = driver.fill_secret(
        &password,
        CredentialField::Password,
        "wrong-origin-secret",
        &wrong_origin,
        &ActContext::default(),
    );
    assert!(
        mismatch.is_err() || matches!(mismatch, Ok(ref result) if !result.status.ok()),
        "wrong origin must not fill successfully: {mismatch:?}"
    );
    assert!(!format!("{mismatch:?}").contains("wrong-origin-secret"));
    let unchanged = driver
        .client_exec("return document.querySelector('#pw').value;", vec![])
        .expect("read password after origin mismatch");
    assert_eq!(unchanged.as_str(), Some(secret));

    let wrong_field = driver.fill_secret(
        &target("Note"),
        CredentialField::Password,
        "field-kind-secret",
        &fixture.origin,
        &ActContext::default(),
    );
    assert!(
        wrong_field.is_err() || matches!(wrong_field, Ok(ref result) if !result.status.ok()),
        "password into a text input must fail: {wrong_field:?}"
    );
    assert!(!format!("{wrong_field:?}").contains("field-kind-secret"));
    let note = driver
        .client_exec("return document.querySelector('#note').value;", vec![])
        .expect("read note field");
    assert_eq!(note.as_str(), Some(""));

    let after_fill = driver
        .observe(&ObservationScope::default())
        .expect("re-observe after fill");
    assert!(
        after_fill.elements.iter().all(|element| {
            !element.name.as_deref().unwrap_or_default().contains(secret)
                && !element
                    .value
                    .as_deref()
                    .unwrap_or_default()
                    .contains(secret)
        }),
        "password secret leaked into the DOM observation"
    );
}

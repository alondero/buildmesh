use super::{GitHubClient, GitHubError};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

struct Response {
    request: &'static str,
    body: Option<serde_json::Value>,
    status: u16,
    response: serde_json::Value,
}

fn server(responses: Vec<Response>) -> (GitHubClient, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), response.request);
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            if let Some(expected) = response.body {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                    expected
                );
            }
            let body = response.response.to_string();
            write!(socket, "HTTP/1.1 {} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.status, body.len(), body).unwrap();
        }
    });
    (GitHubClient::for_test(&base, "test-token").unwrap(), thread)
}

#[test]
fn repository_labels_follow_pages_and_preserve_names() {
    let first = (0..100)
        .map(|i| serde_json::json!({"name": format!("label-{i}")}))
        .collect::<Vec<_>>();
    let (client, thread) = server(vec![
        Response {
            request: "GET /repos/acme/demo/labels?per_page=100&page=1 HTTP/1.1",
            body: None,
            status: 200,
            response: serde_json::json!(first),
        },
        Response {
            request: "GET /repos/acme/demo/labels?per_page=100&page=2 HTTP/1.1",
            body: None,
            status: 200,
            response: serde_json::json!([{"name":"team/ui"}, {"name":"ready-for-agent"}]),
        },
    ]);
    let labels = client.list_repo_labels("acme", "demo").unwrap();
    assert_eq!(labels.len(), 102);
    assert_eq!(&labels[100..], ["team/ui", "ready-for-agent"]);
    thread.join().unwrap();
}

#[test]
fn repository_labels_reject_errors_and_malformed_payloads() {
    for (status, response) in [
        (403, serde_json::json!({"message":"Forbidden"})),
        (200, serde_json::json!([{"color":"abc"}])),
    ] {
        let (client, thread) = server(vec![Response {
            request: "GET /repos/acme/demo/labels?per_page=100&page=1 HTTP/1.1",
            body: None,
            status,
            response,
        }]);
        assert!(client.list_repo_labels("acme", "demo").is_err());
        thread.join().unwrap();
    }
}

#[test]
fn label_mutations_send_one_label_and_encode_removal() {
    let (client, thread) = server(vec![
        Response {
            request: "POST /repos/acme/demo/issues/101/labels HTTP/1.1",
            body: Some(serde_json::json!({"labels":["team/ui"]})),
            status: 200,
            response: serde_json::json!([{"name":"team/ui"}]),
        },
        Response {
            request: "DELETE /repos/acme/demo/issues/101/labels/team%2Fui HTTP/1.1",
            body: None,
            status: 200,
            response: serde_json::json!([]),
        },
        Response {
            request: "POST /repos/acme/demo/issues/101/labels HTTP/1.1",
            body: Some(serde_json::json!({"labels":["missing"]})),
            status: 422,
            response: serde_json::json!({"message":"Label does not exist"}),
        },
    ]);
    client
        .add_issue_label("acme", "demo", 101, "team/ui")
        .unwrap();
    client
        .remove_issue_label("acme", "demo", 101, "team/ui")
        .unwrap();
    assert!(
        matches!(client.add_issue_label("acme", "demo", 101, "missing"), Err(GitHubError::LabelNotFound(label)) if label == "missing")
    );
    thread.join().unwrap();
}

#[test]
fn removal_404_is_success_only_when_an_accessible_issue_confirms_absence() {
    for (read_status, labels, expected_success) in [
        (200, serde_json::json!([]), true),
        (404, serde_json::json!({"message":"Not Found"}), false),
        (200, serde_json::json!([{"name":"team/ui"}]), false),
    ] {
        let (client, thread) = server(vec![
            Response {
                request: "DELETE /repos/acme/demo/issues/101/labels/team%2Fui HTTP/1.1",
                body: None,
                status: 404,
                response: serde_json::json!({"message":"Not Found"}),
            },
            Response {
                request: "GET /repos/acme/demo/issues/101/labels?per_page=100&page=1 HTTP/1.1",
                body: None,
                status: read_status,
                response: labels,
            },
        ]);
        assert_eq!(
            client
                .remove_issue_label_checked("acme", "demo", 101, "team/ui")
                .is_ok(),
            expected_success
        );
        thread.join().unwrap();
    }
}

#[test]
fn worker_removal_keeps_missing_label_idempotent_without_a_verification_read() {
    let (client, thread) = server(vec![Response {
        request: "DELETE /repos/acme/demo/issues/101/labels/team%2Fui HTTP/1.1",
        body: None,
        status: 404,
        response: serde_json::json!({"message":"Not Found"}),
    }]);
    client
        .remove_issue_label("acme", "demo", 101, "team/ui")
        .unwrap();
    thread.join().unwrap();
}

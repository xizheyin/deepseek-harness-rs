#![cfg(any(target_os = "macos", target_os = "linux"))]

mod support;

use std::{
    io::Write,
    os::unix::fs::PermissionsExt as _,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use rustix::process::Signal;

use support::{
    fake_deepseek::{
        BacklogThenStalledSseServer, CancelThenSseServer, DynamicGoalSseServer,
        GatedFirstSseServer, GatedThenStalledSseServer, SequenceSseServer, SplitSseServer,
        StalledSseServer,
    },
    process_state,
    pty::{AutoTuiProfile, DisabledTerminalMode, JobControlHarness, PtyHarness, TestSessionRoot},
};

static WORKSPACE_NUMBER: AtomicUsize = AtomicUsize::new(0);

struct TestWorkspace(std::path::PathBuf);

impl TestWorkspace {
    fn new() -> Self {
        let number = WORKSPACE_NUMBER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("dsh-phase7-pty-{}-{number}", std::process::id()));
        std::fs::create_dir(&path).expect("test workspace should be created");
        Self(path)
    }
}

fn text_sse(text: &str) -> String {
    let text = serde_json::to_string(text).expect("test text should encode");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{text}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
         data: [DONE]\n\n"
    )
}

fn contains_foreground_color_sgr(bytes: &[u8]) -> bool {
    let mut index = 0_usize;
    while index + 2 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let start = index + 2;
        let mut end = start;
        while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        if bytes[end] == b'm' {
            let params = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
            if params.split(';').any(|value| {
                value.parse::<u16>().is_ok_and(|value| {
                    value == 38 || (30..=37).contains(&value) || (90..=97).contains(&value)
                })
            }) {
                return true;
            }
        }
        index = end + 1;
    }
    false
}

fn fragmented_text_sse(fragments: &[&str]) -> String {
    let mut body = String::new();
    body.try_reserve(fragments.len().saturating_mul(96).saturating_add(128))
        .expect("bounded fragmented response should allocate");
    for fragment in fragments {
        let fragment = serde_json::to_string(fragment).expect("test fragment should encode");
        body.push_str("data: {\"choices\":[{\"delta\":{\"content\":");
        body.push_str(&fragment);
        body.push_str("}}]}\n\n");
    }
    body.push_str(concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ));
    body
}

fn request_json(request: &str) -> serde_json::Value {
    let (_, body) = request
        .split_once("\r\n\r\n")
        .expect("HTTP request should contain a body");
    serde_json::from_str(body).expect("HTTP request body should be JSON")
}

fn user_contents(request: &str) -> Vec<String> {
    let request = request_json(request);
    request["messages"]
        .as_array()
        .expect("request messages should be an array")
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| {
            message["content"]
                .as_str()
                .expect("user messages should contain text")
                .to_owned()
        })
        .collect()
}

fn last_user_content(request: &str) -> String {
    user_contents(request)
        .pop()
        .expect("request should contain a user text message")
}

fn tool_message_content(request: &str, call_id: &str) -> String {
    let request = request_json(request);
    request["messages"]
        .as_array()
        .expect("request messages should be an array")
        .iter()
        .find(|message| {
            message["role"] == "tool" && message["tool_call_id"].as_str() == Some(call_id)
        })
        .and_then(|message| message["content"].as_str())
        .expect("request should contain the correlated tool result")
        .to_owned()
}

fn system_message_content(request: &str) -> String {
    let request = request_json(request);
    request["messages"]
        .as_array()
        .expect("request messages should be an array")
        .iter()
        .find(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
        .expect("request should contain a system message")
        .to_owned()
}

fn repeated_text_sse(delta_count: usize) -> String {
    repeated_text_sse_with_width(delta_count, 1)
}

fn repeated_text_sse_with_width(delta_count: usize, text_bytes: usize) -> String {
    let text = "x".repeat(text_bytes);
    let delta = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
        serde_json::to_string(&text).expect("bounded test text should encode")
    );
    let ending = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let mut body = String::new();
    body.try_reserve(delta.len() * delta_count + ending.len())
        .expect("bounded test response should allocate");
    for _ in 0..delta_count {
        body.push_str(&delta);
    }
    body.push_str(ending);
    body
}

fn reasoning_sse(delta_count: usize, final_text: &str) -> String {
    let reasoning_delta =
        concat!("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"r\"}}]}\n\n",);
    let final_text = serde_json::to_string(final_text).expect("test answer should encode");
    let ending = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{final_text}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
         data: [DONE]\n\n"
    );
    let mut body = String::new();
    body.try_reserve(
        reasoning_delta
            .len()
            .saturating_mul(delta_count)
            .saturating_add(ending.len()),
    )
    .expect("bounded reasoning response should allocate");
    for _ in 0..delta_count {
        body.push_str(reasoning_delta);
    }
    body.push_str(&ending);
    body
}

fn text_backlog_then_read_tool_sse(delta_count: usize, text_bytes: usize) -> String {
    let text = "x".repeat(text_bytes);
    let text_delta = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
        serde_json::to_string(&text).expect("bounded test text should encode")
    );
    let tool_delta = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-backlog-read",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": "{\"file_path\":\"note.txt\"}"
                    }
                }]
            }
        }]
    });
    let mut body = String::new();
    body.try_reserve(
        text_delta
            .len()
            .saturating_mul(delta_count)
            .saturating_add(512),
    )
    .expect("bounded backlog response should allocate");
    for _ in 0..delta_count {
        body.push_str(&text_delta);
    }
    body.push_str("data: ");
    body.push_str(&tool_delta.to_string());
    body.push_str(concat!(
        "\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ));
    body
}

fn many_invalid_read_calls_sse(prefix: &str, count: usize, padding_bytes: usize) -> String {
    let mut body = String::new();
    body.try_reserve(count.saturating_mul(padding_bytes.saturating_add(256)))
        .expect("bounded tool-call response should allocate");
    let arguments = serde_json::json!({
        "file_path": "missing.txt",
        "padding": "x".repeat(padding_bytes),
    })
    .to_string();
    assert!(arguments.len() < 256 * 1_024);
    for index in 0..count {
        let delta = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": format!("{prefix}-{index}"),
                        "type": "function",
                        "function": { "name": "read", "arguments": arguments }
                    }]
                }
            }]
        });
        body.push_str("data: ");
        body.push_str(&delta.to_string());
        body.push_str("\n\n");
    }
    body.push_str(concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ));
    body
}

fn tool_sse(call_id: &str, name: &str, arguments: serde_json::Value) -> String {
    let arguments = serde_json::to_string(&arguments).expect("tool arguments should encode");
    let delta = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            }
        }]
    });
    format!(
        "data: {delta}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n\
         data: [DONE]\n\n"
    )
}

fn two_tool_sse(
    first: (&str, &str, serde_json::Value),
    second: (&str, &str, serde_json::Value),
) -> String {
    let calls = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            serde_json::json!({
                "index": index,
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&arguments).unwrap()
                }
            })
        })
        .collect::<Vec<_>>();
    let delta = serde_json::json!({
        "choices": [{ "delta": { "tool_calls": calls } }]
    });
    format!(
        "data: {delta}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n\
         data: [DONE]\n\n"
    )
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for test marker {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn job_control_shell_command() -> &'static str {
    r#"owner=$$;
(
  trap '' HUP INT QUIT TERM
  /bin/sleep 8 & timer=$!
  printf '%s\n' "$timer" > guard.timer
  wait "$timer"
  [ -e guard.cancel ] || kill -KILL -- "-$owner"
) & guard=$!
printf '%s\n' "$guard" > approved.guard.pid
while [ ! -s guard.timer ]; do /bin/sleep 0.01; done
read -r timer < guard.timer
trap ': > cleanup-entered; while [ ! -e cleanup-release ]; do /bin/sleep 0.01; done; : > guard.cancel; kill -KILL "$guard" "$timer" 2>/dev/null; wait "$guard" 2>/dev/null; exit 0' TERM
printf '%s\n' "$owner" > approved.pid
: > shell-started
while :; do /bin/sleep 1; done"#
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn interactive_dsh_streams_before_completion_and_exits_cleanly() {
    let first = concat!("data: {\"choices\":[{\"delta\":{\"content\":\"first-marker \"}}]}\n\n",);
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"second-marker\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let mut server = SplitSseServer::start(first, second);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"stream this\r");
    dsh.expect(b"assistant | first-marker ");
    assert!(
        !dsh.snapshot()
            .windows(b"second-marker".len())
            .any(|window| window == b"second-marker")
    );

    server.release();
    dsh.expect(b"second-marker");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let request = server.finish();

    assert!(status.success());
    assert!(request.contains("\"content\":\"stream this\""));
    assert!(!transcript.contains(&0x1b));
}

#[test]
fn styled_terminal_uses_product_owned_color_and_semantic_labels() {
    let server = SequenceSseServer::start(vec![text_sse("styled answer")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect(b"\x1b[1;36mdsh-rs\x1b[0m");
    dsh.expect("❯".as_bytes());
    dsh.write(b"show the styled interface\r");
    dsh.expect(b"\x1b[1;36mDSH");
    dsh.expect(b"\x1b[36mstyled answer");
    dsh.expect(b"\x1b[1;36mTurn complete");
    dsh.expect_occurrences("❯".as_bytes(), 2);
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert!(transcript.contains(&0x1b));
    assert!(
        !transcript
            .windows(b"assistant |".len())
            .any(|bytes| bytes == b"assistant |")
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn enhanced_command_palette_navigation_resize_and_same_read_exit_are_fenced() {
    let server = SequenceSseServer::start(Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"\x1b[200~/quit\x1b[201~\r");
    dsh.expect(b"Paste ready");
    dsh.expect(b"/quit");
    dsh.write(&[0x15]);

    dsh.write(b"/");
    dsh.expect(b"> /quit");
    dsh.expect(b"/inspect");
    dsh.expect(b"/review");
    dsh.expect(b"/focus");
    dsh.expect(b"/theme");
    dsh.expect(b"/motion");
    dsh.expect(b"/exit");
    dsh.expect(b"/quit");
    dsh.expect(b"/goal");
    dsh.expect(b"/compact");
    dsh.expect("Enter complete · Esc close".as_bytes());
    dsh.write(b"\x1b[A");
    dsh.expect(b"> /exit");

    for (rows, columns, hint) in [
        (12, 44, "Enter complete · Esc close"),
        (5, 12, "Enter · Esc"),
        (15, 80, "Enter complete · Esc close"),
        (24, 112, "Enter complete · Esc close"),
    ] {
        let checkpoint = dsh.checkpoint();
        dsh.resize(rows, columns);
        dsh.expect_after(checkpoint, b"> /exit");
        dsh.expect_after(checkpoint, hint.as_bytes());
    }

    let tab_fenced = dsh.checkpoint();
    dsh.write(b"\t\r");
    dsh.expect_after(tab_fenced, b"> /quit");
    let back_tab = dsh.checkpoint();
    dsh.write(b"\x1b[Z");
    dsh.expect_after(back_tab, b"> /exit");
    let down_fenced = dsh.checkpoint();
    dsh.write(b"\x1b[B\r");
    dsh.expect_after(down_fenced, b"> /quit");
    let completed = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(completed, b"/quit");
    dsh.expect_after(completed, "Enter complete · Esc close".as_bytes());
    dsh.write(b"\r");
    let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));

    assert!(status.success());
    assert!(
        !transcript
            .windows(b"Turn complete".len())
            .any(|window| window == b"Turn complete")
    );
    assert!(server.finish().is_empty());
}

#[test]
fn enhanced_workspace_file_completion_rescans_and_fences_same_read_enter() {
    let server = SequenceSseServer::start(vec![text_sse("file references stayed literal")]);
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("a file.rs"), "safe\n").unwrap();
    std::fs::write(workspace.0.join("b.rs"), "safe\n").unwrap();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"review @");
    dsh.expect(b"> @a file.rs");
    for (rows, columns, expected) in [
        (12, 44, "> @a file.rs"),
        (5, 12, "> @a"),
        (24, 80, "> @a file.rs"),
        (34, 112, "> @a file.rs"),
    ] {
        let checkpoint = dsh.checkpoint();
        dsh.resize(rows, columns);
        dsh.expect_after(checkpoint, expected.as_bytes());
    }

    let first_pick = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(first_pick, b"review @a file.rs ");
    std::fs::write(workspace.0.join("aa-new.rs"), "created later\n").unwrap();

    let second_menu = dsh.checkpoint();
    dsh.write(b"@");
    dsh.expect_after(second_menu, b"> @a file.rs");
    dsh.expect_after(second_menu, b"@aa-new.rs");
    let same_read = dsh.checkpoint();
    dsh.write(b"\x1b[B\r");
    dsh.expect_after(same_read, b"> @aa-new.rs");
    let second_pick = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(second_pick, b"review @a file.rs @aa-new.rs ");

    dsh.write(b"\r");
    dsh.expect(b"file references stayed literal");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "review @a file.rs @aa-new.rs "
    );
}

#[test]
fn enhanced_workspace_file_scan_failure_is_local_and_enter_still_submits() {
    let server = SequenceSseServer::start(vec![text_sse("unavailable menu stayed local")]);
    let workspace = TestWorkspace::new();
    let locked = workspace.0.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"@");
    dsh.expect(b"Workspace files unavailable");
    dsh.write(b"\r");
    dsh.expect(b"unavailable menu stayed local");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "@");
}

#[test]
fn enhanced_streaming_markdown_and_diff_are_styled_without_replaying_source() {
    let body = fragmented_text_sse(&[
        "#",
        " Heading\n``",
        "`diff\n--- a/note\n",
        "+++ b/note\n@@ -1 +1 @@\n",
        "-old-sentinel\n",
        "+新-sentinel\n``",
        "`\nDone with `in",
        "line`.\n| Col",
        "umn | 值 |\n| --- | :---: |\n| alpha | one |\n| beta | two |\n```rust\nfn EOF_CODE() {}\n``",
        "`",
    ]);
    let server = SequenceSseServer::start(vec![body]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"render markdown\r");
    dsh.expect(b"\x1b[1;36mDSH\x1b[0m");
    dsh.expect(b"\x1b[31m-old-sentinel");
    dsh.expect("\x1b[32m+新-sentinel".as_bytes());
    dsh.expect(b"\x1b[1m`inline`");
    dsh.expect(b"\x1b[2;36m|\x1b[0m\x1b[1;36m Column");
    dsh.expect(b"\x1b[2;36m| --- | :---: |");
    dsh.expect(b"\x1b[2;36m|\x1b[0m\x1b[36m alpha");
    dsh.expect(b"\x1b[1mfn EOF_CODE() {}");
    dsh.expect(b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    for sentinel in [
        "-old-sentinel",
        "+新-sentinel",
        "`inline`",
        " Column ",
        " alpha ",
        "fn EOF_CODE() {}",
    ] {
        assert_eq!(
            transcript
                .windows(sentinel.len())
                .filter(|window| *window == sentinel.as_bytes())
                .count(),
            1,
            "{sentinel} should enter native scrollback once"
        );
    }
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn linear_tui_keeps_markdown_literal_and_emits_no_escape_bytes() {
    let answer = "# Heading\n```diff\n-old-linear\n+new-linear\n```\n`inline`\n| Column | Value |\n| --- | --- |\n| alpha | one |\n";
    let server = SequenceSseServer::start(vec![text_sse(answer)]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"render plain markdown\r");
    dsh.expect(b"# Heading");
    dsh.expect(b"```diff");
    dsh.expect(b"-old-linear");
    dsh.expect(b"+new-linear");
    dsh.expect(b"`inline`");
    dsh.expect(b"| Column | Value |");
    dsh.expect(b"| --- | --- |");
    dsh.expect(b"| alpha | one |");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn linear_tui_treats_at_paths_as_literal_and_never_opens_a_dynamic_menu() {
    let server = SequenceSseServer::start(vec![text_sse("literal at path")]);
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("note.txt"), "safe\n").unwrap();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"read @note.txt literally\r");
    dsh.expect(b"literal at path");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert!(
        !transcript
            .windows(b"Scanning workspace".len())
            .any(|window| { window == b"Scanning workspace" })
    );
    assert!(
        !transcript
            .windows(b"Workspace files".len())
            .any(|window| { window == b"Workspace files" })
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "read @note.txt literally");
}

#[test]
fn linear_inspect_and_review_are_local_zero_escape_reports() {
    let server = SequenceSseServer::start(vec![text_sse("linear detail source answer")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"build a linear review\r");
    dsh.expect(b"linear detail source answer");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);

    dsh.write(b"/review\r");
    dsh.expect(b"REVIEW");
    dsh.expect(b"Turn complete");
    dsh.expect(b"0 tool requests");
    dsh.expect_occurrences(b"dsh > ", 3);

    dsh.write(b"/inspect\r");
    dsh.expect(b"INSPECT");
    dsh.expect(b"COMMITTED FACTS");
    dsh.expect_occurrences(b"dsh > ", 4);

    dsh.write(b"/theme paper\r");
    dsh.expect(b"[linear UI is always plain; theme command kept local]");
    dsh.expect_occurrences(b"dsh > ", 5);
    dsh.write(b"/theme PRIVATE_UNKNOWN_NAME\r");
    dsh.expect(b"[unknown theme; linear UI remains plain]");
    dsh.expect_occurrences(b"dsh > ", 6);
    dsh.write(b"/motion reduced\r");
    dsh.expect(b"[linear UI has no periodic animation]");
    dsh.expect_occurrences(b"dsh > ", 7);
    dsh.write(b"/motion PRIVATE_UNKNOWN_NAME\r");
    dsh.expect(b"[unknown motion mode; linear UI has no periodic animation]");
    dsh.expect_occurrences(b"dsh > ", 8);
    dsh.write(b"/motions\r");
    dsh.expect(b"[unknown motion mode; linear UI has no periodic animation]");
    dsh.expect_occurrences(b"dsh > ", 9);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "build a linear review");
}

#[test]
fn enhanced_working_spinner_switches_to_static_reduced_motion_without_model_input() {
    let server = StalledSseServer::start(String::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    let turn = dsh.checkpoint();
    dsh.write(b"keep this turn pending\r");
    dsh.expect_after(turn, "● | Working".as_bytes());
    dsh.expect_after(turn, "● / Working".as_bytes());
    let animated_tail = dsh.snapshot()[turn..].to_vec();
    let static_at = animated_tail
        .windows("● Working".len())
        .position(|window| window == "● Working".as_bytes())
        .expect("the stable semantic icon should render before animation");
    let phase_at = animated_tail
        .windows("● | Working".len())
        .position(|window| window == "● | Working".as_bytes())
        .expect("the delayed first phase should render");
    assert!(static_at < phase_at);

    let changed = dsh.checkpoint();
    dsh.write(b"/motion reduced\r");
    dsh.expect_after(changed, "Motion changed · reduced".as_bytes());
    let invalid = dsh.checkpoint();
    dsh.write(b"/Motion reduced\r");
    dsh.expect_after(invalid, b"Unknown motion mode");
    let static_checkpoint = dsh.checkpoint();
    dsh.write(b"x");
    dsh.expect_after(static_checkpoint, "● Working".as_bytes());
    let static_checkpoint = dsh.checkpoint();
    std::thread::sleep(Duration::from_millis(500));
    let static_tail = dsh.snapshot()[static_checkpoint..].to_vec();
    for phase in ["● | Working", "● / Working", "● - Working", "● \\ Working"] {
        assert!(
            !static_tail
                .windows(phase.len())
                .any(|window| window == phase.as_bytes()),
            "reduced motion must not emit periodic phase frames"
        );
    }

    let cancelled = dsh.checkpoint();
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    dsh.expect_after(cancelled, b"Ready");
    dsh.write(&[0x15]);
    let (status, _) = dsh.exit_cleanly();
    let (request, closed) = server.finish();

    assert!(status.success());
    assert!(closed);
    assert_eq!(last_user_content(&request), "keep this turn pending");
    assert!(!request.contains("/motion"));
}

#[test]
fn reduced_motion_flag_is_static_in_enhanced_and_inert_in_zero_escape_linear_ui() {
    let enhanced_server = StalledSseServer::start(String::new());
    let enhanced_workspace = TestWorkspace::new();
    let mut enhanced =
        PtyHarness::spawn_reduced_motion(&enhanced_server.base_url, &enhanced_workspace.0, true);

    enhanced.expect("❯".as_bytes());
    let turn = enhanced.checkpoint();
    enhanced.write(b"start reduced\r");
    enhanced.expect_after(turn, "● Working".as_bytes());
    std::thread::sleep(Duration::from_millis(500));
    let tail = enhanced.snapshot()[turn..].to_vec();
    for phase in ["● | Working", "● / Working", "● - Working", "● \\ Working"] {
        assert!(
            !tail
                .windows(phase.len())
                .any(|window| window == phase.as_bytes())
        );
    }
    let cancelled = enhanced.checkpoint();
    enhanced.write(&[0x03]);
    enhanced.expect(b"stopped; skipped");
    enhanced.expect_after(cancelled, b"Ready");
    let (enhanced_status, _) = enhanced.exit_cleanly();
    let (request, closed) = enhanced_server.finish();
    assert!(enhanced_status.success());
    assert!(closed);
    assert_eq!(last_user_content(&request), "start reduced");

    let linear_server = SequenceSseServer::start(Vec::new());
    let linear_workspace = TestWorkspace::new();
    let mut linear =
        PtyHarness::spawn_reduced_motion(&linear_server.base_url, &linear_workspace.0, false);
    linear.expect(b"dsh > ");
    linear.write(b"/motion full\r");
    linear.expect(b"[linear UI has no periodic animation]");
    linear.expect_occurrences(b"dsh > ", 2);
    let (linear_status, transcript) = linear.exit_cleanly();
    assert!(linear_status.success());
    assert!(!transcript.contains(&0x1b));
    assert!(linear_server.finish().is_empty());
}

#[test]
fn enhanced_theme_commands_are_local_transactional_and_reach_all_six_palettes() {
    let mut server = GatedFirstSseServer::start(
        String::new(),
        text_sse("theme selection kept one fresh request"),
        Vec::new(),
    );
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    for (name, sgr) in [
        ("midnight", "\x1b[1;38;5;221m"),
        ("paper", "\x1b[1;38;5;130m"),
        ("color-blind", "\x1b[1;38;5;208m"),
        ("high-contrast", "\x1b[1;7m"),
        ("mono", "\x1b[1m"),
        ("adaptive", "\x1b[1;33m"),
    ] {
        let checkpoint = dsh.checkpoint();
        dsh.write(format!("/theme {name}\r").as_bytes());
        dsh.expect_after(checkpoint, format!("Theme changed · {name}").as_bytes());
        dsh.expect_after(checkpoint, sgr.as_bytes());
        server.assert_no_first_request(Duration::from_millis(100));
        if name == "paper" {
            let narrow = dsh.checkpoint();
            dsh.resize(20, 44);
            dsh.expect_after(narrow, b"\x1b[1;38;5;25m");
            let wide = dsh.checkpoint();
            dsh.resize(24, 80);
            dsh.expect_after(wide, b"\x1b[1;38;5;25m");
        }
    }

    let wide_list = dsh.checkpoint();
    dsh.resize(34, 112);
    dsh.expect_after(wide_list, b"\x1b[2;36m");
    let list = dsh.checkpoint();
    dsh.write(b"/theme\r");
    dsh.expect_after(list, "Theme · adaptive".as_bytes());
    dsh.expect_after(list, b"color-blind");
    dsh.expect_after(list, b"high-contrast");
    server.assert_no_first_request(Duration::from_millis(100));

    let complete_theme_tail = "high-contrast · mono".as_bytes();
    for (rows, columns, visible_notice) in [
        (20, 44, "Theme · adaptive | Themes · adaptive · midn"),
        (
            24,
            80,
            "Theme · adaptive | Themes · adaptive · midnight · paper · color-blind · high-co",
        ),
        (5, 12, "Theme · ada"),
    ] {
        let resized = dsh.checkpoint();
        dsh.resize(rows, columns);
        dsh.expect_after(resized, b"\x1b[2;36m");
        let show = dsh.checkpoint();
        dsh.write(b"/theme\r");
        dsh.expect_after(show, visible_notice.as_bytes());
        assert!(
            !dsh.snapshot()[show..]
                .windows(complete_theme_tail.len())
                .any(|window| window == complete_theme_tail),
            "a narrow one-line Dock notice must be visibly truncated"
        );
        server.assert_no_first_request(Duration::from_millis(100));
    }

    let restored = dsh.checkpoint();
    dsh.resize(34, 112);
    dsh.expect_after(restored, b"\x1b[2;36m");

    let invalid = dsh.checkpoint();
    dsh.write(b"/theme PRIVATE_THEME_NAME\r");
    dsh.expect_after(invalid, b"Unknown theme");
    server.assert_no_first_request(Duration::from_millis(100));
    assert!(
        !dsh.snapshot()[invalid..]
            .windows(b"PRIVATE_THEME_NAME".len())
            .any(|window| window == b"PRIVATE_THEME_NAME")
    );

    let fence = dsh.checkpoint();
    dsh.write(b"/theme paper\rHIDDEN_THEME_PROMPT\r");
    dsh.expect_after(fence, "Theme changed · paper".as_bytes());
    server.assert_no_first_request(Duration::from_millis(250));

    dsh.write(b"fresh request after theme fence\r");
    server.release();
    dsh.expect(b"theme selection kept one fresh request");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "fresh request after theme fence"
    );
}

#[test]
fn active_palette_theme_and_unknown_slash_keep_the_next_turn_fifo_truthful() {
    let first =
        "data: {\"choices\":[{\"delta\":{\"content\":\"active theme prefix\"}}]}\n\n".to_owned();
    let mut server = GatedThenStalledSseServer::start(first, text_sse(" active theme suffix"));
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start a themed active turn\r");
    dsh.expect(b"active theme prefix");
    let help = dsh.checkpoint();
    dsh.write(b"/he");
    dsh.expect_after(help, b"> /help");
    dsh.write(b"\r\r");
    dsh.expect_after(help, b"/help");
    server.assert_no_second_request(Duration::from_millis(150));
    dsh.write(b"\r");
    dsh.expect_after(help, b"/inspect | /review | /focus");
    server.assert_no_second_request(Duration::from_millis(150));

    let themed = dsh.checkpoint();
    dsh.write(b"/theme mono\r");
    dsh.expect_after(themed, "Theme changed · mono".as_bytes());
    server.assert_no_second_request(Duration::from_millis(250));

    let shown = dsh.checkpoint();
    dsh.write(b"/theme\r");
    dsh.expect_after(shown, "Theme · mono".as_bytes());
    server.assert_no_second_request(Duration::from_millis(100));

    let invalid = dsh.checkpoint();
    dsh.write(b"/theme PRIVATE_ACTIVE_THEME\rHIDDEN_ACTIVE_PROMPT\r");
    dsh.expect_after(invalid, b"Unknown theme");
    server.assert_no_second_request(Duration::from_millis(250));
    assert!(
        !dsh.snapshot()[invalid..]
            .windows(b"PRIVATE_ACTIVE_THEME".len())
            .any(|window| window == b"PRIVATE_ACTIVE_THEME")
    );

    server.release();
    dsh.expect_after(themed, b"active theme suffix");
    dsh.expect_after(themed, b"Turn complete");
    server.assert_no_second_request(Duration::from_millis(250));
    let themed_output = dsh.snapshot();
    assert_eq!(
        themed_output
            .windows(b"active theme prefix".len())
            .filter(|window| *window == b"active theme prefix")
            .count(),
        1,
        "a palette-only redraw must not replay old transcript text"
    );
    assert!(
        !contains_foreground_color_sgr(&themed_output[themed..]),
        "Mono may use attributes and cursor controls, but no foreground color"
    );

    let unknown = dsh.checkpoint();
    dsh.write(b"/not-local");
    dsh.expect_after(unknown, b"No matching local command");
    dsh.write(b"\r");
    server.wait_until_second_request();
    dsh.expect(b"Working");
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    let (status, _) = dsh.exit_cleanly();
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        last_user_content(&requests[0]),
        "start a themed active turn"
    );
    assert_eq!(last_user_content(&requests[1]), "/not-local");
    assert_eq!(
        user_contents(&requests[1]),
        ["start a themed active turn", "/not-local"]
    );
}

#[test]
fn active_workspace_file_completion_queues_only_after_a_fresh_enter() {
    let first =
        "data: {\"choices\":[{\"delta\":{\"content\":\"active file prefix\"}}]}\n\n".to_owned();
    let mut server = GatedThenStalledSseServer::start(first, text_sse(" active file suffix"));
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("note.txt"), "safe\n").unwrap();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start active file turn\r");
    dsh.expect(b"active file prefix");
    let menu = dsh.checkpoint();
    dsh.write(b"next @");
    dsh.expect_after(menu, b"> @note.txt");
    let completed = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(completed, b"next @note.txt ");
    server.assert_no_second_request(Duration::from_millis(150));
    let queued = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(queued, b"1 next-turn prompt(s) queued");
    server.assert_no_second_request(Duration::from_millis(150));

    server.release();
    dsh.expect(b"active file suffix");
    dsh.expect(b"Turn complete");
    server.wait_until_second_request();
    dsh.expect(b"Working");
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    let (status, _) = dsh.exit_cleanly();
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(last_user_content(&requests[0]), "start active file turn");
    assert_eq!(last_user_content(&requests[1]), "next @note.txt ");
}

#[test]
fn active_exit_aliases_complete_then_cancel_and_clean_up_the_turn() {
    for (prefix, command) in [(b"/ex".as_slice(), b"/exit".as_slice()), (b"/qu", b"/quit")] {
        let partial =
            concat!("data: {\"choices\":[{\"delta\":{\"content\":\"active exit cleanup\"}}]}\n\n")
                .to_owned();
        let server = StalledSseServer::start(partial);
        let workspace = TestWorkspace::new();
        let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

        dsh.expect("❯".as_bytes());
        dsh.write(b"keep provider open while local exit runs\r");
        dsh.expect(b"active exit cleanup");
        let queued = dsh.checkpoint();
        dsh.write(b"DO_NOT_ADMIT_AFTER_LOCAL_EXIT\r");
        dsh.expect_after(queued, b"next-turn prompt(s) queued");
        let palette = dsh.checkpoint();
        dsh.write(prefix);
        dsh.expect_after(palette, command);
        let completion = dsh.checkpoint();
        dsh.write(b"\r\r");
        let mut completed_prompt = "❯ ".as_bytes().to_vec();
        completed_prompt.extend_from_slice(command);
        dsh.expect_after(completion, &completed_prompt);
        let exact = dsh.checkpoint();
        dsh.write(b"\r");
        let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));
        let (request, closed) = server.finish();

        assert!(status.success());
        assert!(closed, "local exit must close the active provider request");
        assert_eq!(
            last_user_content(&request),
            "keep provider open while local exit runs"
        );
        assert_eq!(
            user_contents(&request),
            ["keep provider open while local exit runs"]
        );
        assert!(
            !transcript[exact..]
                .windows(b"Turn complete".len())
                .any(|window| window == b"Turn complete"),
            "an active local exit is cancellation, not a completed turn"
        );
    }
}

#[test]
fn explicit_linear_and_enhanced_tui_modes_reach_their_real_terminal_paths() {
    let server = SequenceSseServer::start(Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color_with_tui_mode(&server.base_url, &workspace.0, "linear");

    dsh.expect(b"dsh > ");
    assert!(!dsh.terminal_uses_application_mode());
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert!(server.finish().is_empty());

    let server = SequenceSseServer::start(Vec::new());
    let mut enhanced =
        PtyHarness::spawn_color_with_tui_mode(&server.base_url, &workspace.0, "enhanced");
    enhanced.expect(b"Ready");
    assert!(enhanced.terminal_uses_application_mode());
    let (status, transcript) = enhanced.exit_cleanly();
    assert!(status.success());
    assert!(transcript.contains(&0x1b));
    assert!(server.finish().is_empty());
}

#[test]
fn auto_tui_profile_is_conservative_for_each_real_cli_override() {
    struct Case {
        term: &'static str,
        environment: Option<(&'static str, &'static str)>,
        size: (u16, u16),
        no_color_argument: bool,
        no_color_environment: bool,
    }

    for case in [
        Case {
            term: "xterm-256color",
            environment: None,
            size: (24, 120),
            no_color_argument: true,
            no_color_environment: false,
        },
        Case {
            term: "xterm-256color",
            environment: None,
            size: (24, 120),
            no_color_argument: false,
            no_color_environment: true,
        },
        Case {
            term: "dumb",
            environment: None,
            size: (24, 120),
            no_color_argument: false,
            no_color_environment: false,
        },
        Case {
            term: "vt100",
            environment: None,
            size: (24, 120),
            no_color_argument: false,
            no_color_environment: false,
        },
        Case {
            term: "xterm-256color",
            environment: Some(("ZELLIJ", "0")),
            size: (24, 120),
            no_color_argument: false,
            no_color_environment: false,
        },
        Case {
            term: "xterm-256color",
            environment: None,
            size: (12, 43),
            no_color_argument: false,
            no_color_environment: false,
        },
    ] {
        let server = SequenceSseServer::start(Vec::new());
        let workspace = TestWorkspace::new();
        let mut dsh = PtyHarness::spawn_auto_with_profile(
            &server.base_url,
            &workspace.0,
            AutoTuiProfile {
                term: case.term,
                environment: case.environment,
                size: case.size,
                no_color_argument: case.no_color_argument,
                no_color_environment: case.no_color_environment,
                enhanced: false,
            },
        );

        dsh.expect(b"dsh > ");
        assert!(!dsh.terminal_uses_application_mode());
        let (status, transcript) = dsh.exit_cleanly();
        assert!(status.success());
        assert!(!transcript.contains(&0x1b));
        assert!(server.finish().is_empty());
    }
}

#[test]
fn enhanced_dock_keeps_cbreak_across_idle_and_restores_it_for_suspend() {
    let server = SequenceSseServer::start(vec![text_sse("answer after enhanced suspension")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect(b"Ready");
    dsh.expect("❯".as_bytes());
    assert!(dsh.terminal_uses_application_mode());
    assert_ne!(dsh.terminal_state(), dsh.initial_terminal_state());
    dsh.write(b"/theme paper\r");
    dsh.expect("Theme changed · paper".as_bytes());
    dsh.write(b"continue after enhanced suspension");
    let inspect = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(inspect, b"INSPECT");

    dsh.signal(Signal::TSTP);
    dsh.wait_until_stopped();
    assert_eq!(dsh.terminal_state(), dsh.initial_terminal_state());
    let resumed = dsh.checkpoint();
    dsh.signal(Signal::CONT);
    dsh.expect_after(resumed, b"INSPECT");
    dsh.expect_after(resumed, b"\x1b[1;38;5;25m");
    assert!(dsh.terminal_uses_application_mode());

    let turn_checkpoint = dsh.checkpoint();
    dsh.write(b"\x1b");
    dsh.expect_after(turn_checkpoint, b"continue after enhanced suspension");
    dsh.write(b"\r");
    dsh.expect(b"answer after enhanced suspension");
    dsh.expect_after(turn_checkpoint, b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "continue after enhanced suspension"
    );
}

#[test]
fn enhanced_idle_hup_quit_and_term_restore_exact_terminal_state_before_exit() {
    for (signal, expected) in [(Signal::HUP, 129), (Signal::QUIT, 131), (Signal::TERM, 143)] {
        let server = SequenceSseServer::start(Vec::new());
        let workspace = TestWorkspace::new();
        let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

        dsh.expect(b"Ready");
        assert!(dsh.terminal_uses_application_mode());
        dsh.signal(signal);
        let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));

        assert_eq!(status.code(), Some(expected));
        assert!(server.finish().is_empty());
    }
}

#[test]
fn enhanced_active_ctrl_c_and_ctrl_d_cancel_the_request_and_restore_the_terminal() {
    for (input, exits_after_cancel) in [(vec![0x03], false), (vec![0x04], true)] {
        let partial =
            concat!("data: {\"choices\":[{\"delta\":{\"content\":\"enhanced-stall\"}}]}\n\n")
                .to_owned();
        let server = StalledSseServer::start(partial);
        let workspace = TestWorkspace::new();
        let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

        dsh.expect("❯".as_bytes());
        dsh.write(b"stall the enhanced turn\r");
        dsh.expect(b"enhanced-stall");
        dsh.write(&input);
        let (status, _) = if exits_after_cancel {
            dsh.wait_for_exit(Duration::from_secs(5))
        } else {
            dsh.expect(b"stopped; skipped");
            dsh.exit_cleanly()
        };
        let (request, closed) = server.finish();

        assert!(status.success());
        assert!(closed);
        assert!(request.contains("stall the enhanced turn"));
    }
}

#[test]
fn enhanced_ctrl_c_keeps_the_screen_ledger_usable_for_the_next_turn() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"cancelled-partial\"}}]}\n\n")
            .to_owned();
    let server = CancelThenSseServer::start(partial, text_sse("second turn still aligned"));
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"cancel this enhanced turn\r");
    dsh.expect(b"cancelled-partial");
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    dsh.expect("❯".as_bytes());
    dsh.write(b"continue after cancellation\r");
    dsh.expect(b"second turn still aligned");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let (requests, first_closed) = server.finish();

    assert!(status.success());
    assert!(first_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(last_user_content(&requests[0]), "cancel this enhanced turn");
    assert_eq!(
        last_user_content(&requests[1]),
        "continue after cancellation"
    );
}

#[test]
fn enhanced_ctrl_c_flushes_a_partial_fence_as_plain_before_the_next_turn() {
    let fragment = serde_json::to_string("visible-before-cancel\n```rust\ncancelled-fence")
        .expect("test fragment should encode");
    let partial = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{fragment}}}}}]}}\n\n");
    let server = CancelThenSseServer::start(partial, text_sse("after cancelled fence"));
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"cancel an open fence\r");
    dsh.expect(b"visible-before-cancel");
    dsh.write(&[0x03]);
    dsh.expect(b"cancelled-fence");
    dsh.expect(b"stopped; skipped");
    dsh.expect("❯".as_bytes());
    dsh.write(b"continue after the open fence\r");
    dsh.expect(b"after cancelled fence");
    dsh.expect(b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();
    let (requests, first_closed) = server.finish();

    assert!(status.success());
    assert!(first_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        transcript
            .windows(b"cancelled-fence".len())
            .filter(|window| *window == b"cancelled-fence")
            .count(),
        1
    );
    assert_eq!(
        last_user_content(&requests[1]),
        "continue after the open fence"
    );
}

#[test]
fn enhanced_composer_edits_fragmented_unicode_and_distinguishes_ctrl_j_from_enter() {
    let server = SequenceSseServer::start(vec![text_sse("unicode composer accepted")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    let turn_checkpoint = dsh.checkpoint();
    dsh.write(b"A");
    for byte in "👨‍👩‍👧‍👦".as_bytes() {
        dsh.write(&[*byte]);
    }
    dsh.write(b"B\x1b[D\x7f");
    for byte in "界".as_bytes() {
        dsh.write(&[*byte]);
    }
    dsh.write(&[0x05]);
    dsh.write(b"\ntail\r");
    dsh.expect(b"unicode composer accepted");
    dsh.expect_after(turn_checkpoint, b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "A界B\ntail");
}

#[test]
fn enhanced_inspect_and_review_are_local_read_only_panels_that_preserve_the_draft() {
    let mut server = GatedFirstSseServer::start(
        String::new(),
        text_sse("detail views kept the draft"),
        Vec::new(),
    );
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"draft survives detail views");
    let inspect = dsh.checkpoint();
    dsh.write(b"\x0f\r");
    dsh.expect_after(inspect, b"INSPECT");
    dsh.expect_after(inspect, b"COMMITTED FACTS");
    server.assert_no_first_request(Duration::from_millis(250));

    for (rows, columns) in [(20, 44), (34, 112), (24, 80)] {
        let resize = dsh.checkpoint();
        dsh.resize(rows, columns);
        dsh.expect_after(resize, b"INSPECT");
    }
    let compact = dsh.checkpoint();
    dsh.resize(20, 43);
    dsh.expect_after(compact, b"draft survives detail views");
    let regrow = dsh.checkpoint();
    dsh.resize(34, 112);
    dsh.expect_after(regrow, b"draft survives detail views");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !dsh.snapshot()[regrow..]
            .windows(b"INSPECT".len())
            .any(|window| window == b"INSPECT"),
        "regrowing must not reopen a detail panel without a fresh command"
    );
    let reopen = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(reopen, b"INSPECT");

    let paste = dsh.checkpoint();
    dsh.write(b"\x1b[200~EVIL\rPROMPT\x1b[201~\r");
    dsh.expect_after(paste, b"INSPECT");
    server.assert_no_first_request(Duration::from_millis(250));

    let review = dsh.checkpoint();
    dsh.write(b"\t");
    dsh.expect_after(review, b"REVIEW");
    dsh.expect_after(review, b"Complete a turn before opening Review");

    let focus = dsh.checkpoint();
    dsh.write(b"\x1b");
    dsh.expect_after(focus, b"draft survives detail views");
    server.assert_no_first_request(Duration::from_millis(250));
    dsh.write(b"\r");
    dsh.expect(b"Working");
    server.release();
    dsh.expect(b"detail views kept the draft");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "draft survives detail views"
    );
}

#[test]
fn enhanced_reasoning_moves_from_focus_into_live_inspect() {
    let partial = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"PRIVATE_REASONING_SENTINEL\"}}]}\n\n"
    )
    .to_owned();
    let finish = text_sse("reasoning stayed out of Focus");
    let mut server = GatedFirstSseServer::start(partial, finish, Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    let focus_start = dsh.checkpoint();
    dsh.write(b"inspect the private reasoning\r");
    dsh.expect_after(focus_start, b"Working");
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        !dsh.snapshot()[focus_start..]
            .windows(b"PRIVATE_REASONING_SENTINEL".len())
            .any(|window| window == b"PRIVATE_REASONING_SENTINEL"),
        "Focus must not print reasoning text"
    );

    let inspect = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(inspect, b"INSPECT");
    dsh.expect_after(inspect, b"PRIVATE_REASONING_SENTINEL");
    server.release();
    dsh.expect(b"reasoning stayed out of Focus");
    dsh.expect(b"Turn complete");
    let review = dsh.checkpoint();
    dsh.write(b"\t");
    dsh.expect_after(review, b"REVIEW");
    dsh.expect_after(review, b"0 tool requests");
    let focus = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(focus, b"Ready");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "inspect the private reasoning"
    );
}

#[test]
fn active_inspect_scroll_resize_and_same_read_enter_never_queue_a_hidden_prompt() {
    let mut reasoning = String::new();
    for index in 0..80 {
        std::fmt::Write::write_fmt(&mut reasoning, format_args!("REASONING_{index:02}\n"))
            .expect("bounded test reasoning should format");
    }
    let encoded = serde_json::to_string(&reasoning).expect("test reasoning should encode");
    let partial =
        format!("data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":{encoded}}}}}]}}\n\n");
    let mut server =
        GatedThenStalledSseServer::start(partial, text_sse("first active detail turn complete"));
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"inspect while the turn is active\r");
    dsh.expect(b"Working");
    let local_inspect = dsh.checkpoint();
    dsh.write(b"/inspect\r");
    dsh.expect_after(local_inspect, b"INSPECT");
    let local_focus = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(local_focus, b"Working");
    let local_review = dsh.checkpoint();
    dsh.write(b"/review\r");
    dsh.expect_after(local_review, b"REVIEW");
    let review_focus = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(review_focus, b"Working");
    let local_focus = dsh.checkpoint();
    dsh.write(b"/focus\r");
    dsh.expect_after(local_focus, b"Working");
    dsh.write(b"SAFE_NEXT_PROMPT");
    let inspect = dsh.checkpoint();
    dsh.write(b"\x0f\r");
    dsh.expect_after(inspect, b"INSPECT");
    dsh.expect_after(inspect, b"REASONING_00");

    let end = dsh.checkpoint();
    dsh.write(b"\x1b[F");
    dsh.expect_after(end, b"REASONING_79");
    for (rows, columns) in [(20, 44), (34, 112), (24, 80)] {
        let resize = dsh.checkpoint();
        dsh.resize(rows, columns);
        dsh.expect_after(resize, b"INSPECT");
    }

    server.release();
    dsh.expect(b"first active detail turn complete");
    dsh.expect(b"Turn complete");
    server.assert_no_second_request(Duration::from_millis(250));

    let focus = dsh.checkpoint();
    dsh.write(b"\x1b");
    dsh.expect_after(focus, b"SAFE_NEXT_PROMPT");
    server.assert_no_second_request(Duration::from_millis(250));
    dsh.write(b"\r");
    server.wait_until_second_request();
    dsh.expect(b"Working");
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    let (status, _) = dsh.exit_cleanly();
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        last_user_content(&requests[0]),
        "inspect while the turn is active"
    );
    assert_eq!(last_user_content(&requests[1]), "SAFE_NEXT_PROMPT");
    assert_eq!(
        user_contents(&requests[1]),
        ["inspect while the turn is active", "SAFE_NEXT_PROMPT"]
    );
}

#[test]
fn enhanced_resize_reanchors_the_full_screen_dock_and_preserves_the_draft() {
    let server = SequenceSseServer::start(vec![text_sse("resized draft accepted")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"draft survives resize");

    let wide = dsh.checkpoint();
    dsh.resize(30, 80);
    dsh.expect_after(wide, b"draft survives resize");

    let compact = dsh.checkpoint();
    dsh.resize(20, 44);
    dsh.expect_after(compact, b"draft survives resize");

    let rescue = dsh.checkpoint();
    dsh.resize(6, 15);
    dsh.expect_after(rescue, b"^ ves resize");
    assert!(dsh.terminal_uses_application_mode());

    let restored = dsh.checkpoint();
    dsh.resize(24, 120);
    dsh.expect_after(restored, b"draft survives resize");
    assert!(dsh.terminal_uses_application_mode());
    assert!(
        !dsh.snapshot()
            .windows(b"\x1b[1;21r".len())
            .any(|window| window == b"\x1b[1;21r"),
        "enhanced mode must not establish a partial scrolling region"
    );
    dsh.write(b"\r");
    dsh.expect(b"resized draft accepted");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "draft survives resize");
}

#[test]
fn enhanced_resize_below_the_compact_floor_clears_stale_geometry_before_exit() {
    let server = SequenceSseServer::start(Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"private draft stays in the viewport");
    let resize = dsh.checkpoint();
    dsh.resize(5, 11);
    let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));

    assert_eq!(status.code(), Some(1));
    assert!(
        transcript[resize..]
            .windows(b"\x1b[2J".len())
            .any(|window| window == b"\x1b[2J")
    );
    assert!(
        transcript[resize..]
            .windows(b"\x1b[?2004l".len())
            .any(|window| window == b"\x1b[?2004l")
    );
    assert!(server.finish().is_empty());
}

#[test]
fn fragmented_bracketed_paste_is_one_draft_and_never_submits_its_enter_bytes() {
    let server = SequenceSseServer::start(vec![text_sse("atomic paste accepted")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    let turn_checkpoint = dsh.checkpoint();
    for byte in b"\x1b[200~" {
        dsh.write(&[*byte]);
    }
    dsh.write(b"first\rsecond\x1b[A\nthird");
    for byte in b"\x1b[201~" {
        dsh.write(&[*byte]);
    }
    dsh.expect(b"third");
    dsh.expect(b"Paste ready");
    dsh.write(b"\r");
    dsh.expect(b"atomic paste accepted");
    dsh.expect_after(turn_checkpoint, b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        last_user_content(&requests[0]),
        "first\nsecond\u{1b}[A\nthird"
    );
}

#[test]
fn a_completed_paste_fence_discards_a_later_read_before_enter_can_submit() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"paste guard answer\"}}]}\n\n")
            .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedFirstSseServer::start(partial, finish, Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"\x1b[200~safe paste\x1b[201~");
    dsh.expect(b"Paste inserted");
    dsh.write(b"\rhidden suffix\x1b[201~");
    server.assert_no_first_request(Duration::from_millis(250));

    dsh.expect(b"Paste ready");
    dsh.write(b"\r");
    dsh.expect(b"paste guard answer");
    server.release();
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "safe paste");
}

#[test]
fn a_rejected_oversized_paste_fence_cannot_submit_the_existing_draft() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"rejected paste answer\"}}]}\n\n")
            .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedFirstSseServer::start(partial, finish, Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"safe draft");
    let mut paste = Vec::from(b"\x1b[200~".as_slice());
    paste.extend(std::iter::repeat_n(b'x', 64 * 1_024 + 1));
    paste.extend_from_slice(b"\x1b[201~");
    dsh.write(&paste);
    dsh.expect(b"CLI_INPUT_PASTE_TOO_LARGE");
    dsh.write(b"\r");
    server.assert_no_first_request(Duration::from_millis(250));

    dsh.expect(b"Paste ready");
    dsh.write(b"\r");
    dsh.expect(b"rejected paste answer");
    server.release();
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "safe draft");
}

#[test]
fn rejected_escape_sequence_discards_the_same_read_enter_without_losing_the_draft() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"invalid guard answer\"}}]}\n\n")
            .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedFirstSseServer::start(partial, finish, Vec::new());
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"safe draft");
    dsh.write(b"\x1b[999~\r");
    dsh.expect(b"CLI_INPUT_UNKNOWN_SEQUENCE");
    server.assert_no_first_request(Duration::from_millis(200));

    dsh.write(b"\r");
    dsh.expect(b"invalid guard answer");
    server.release();
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "safe draft");
}

#[test]
fn input_typed_during_a_turn_is_queued_and_admitted_only_after_settlement() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"busy-partial\"}}]}\n\n").to_owned();
    let mut server = GatedFirstSseServer::start(
        partial,
        text_sse(" first-turn-finished"),
        vec![text_sse("queued-turn-finished")],
    );
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    let journey_checkpoint = dsh.checkpoint();
    dsh.write(b"start the gated turn\r");
    dsh.expect(b"busy-partial");
    let queue_checkpoint = dsh.checkpoint();
    dsh.write(b"run this only after settlement\r");
    dsh.expect_after(queue_checkpoint, b"next-turn prompt(s) queued");
    server.release();
    dsh.expect(b"first-turn-finished");
    dsh.expect(b"1 item");
    dsh.expect(b"queued-turn-finished");
    dsh.expect_after(journey_checkpoint, b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(last_user_content(&requests[0]), "start the gated turn");
    assert_eq!(
        last_user_content(&requests[1]),
        "run this only after settlement"
    );
    assert!(!requests[0].contains("run this only after settlement"));
}

#[test]
fn goal_command_runs_sequential_rounds_until_the_model_completes_it() {
    let server = DynamicGoalSseServer::start(
        vec![text_sse("goal round one finished")],
        text_sse("goal is complete"),
    );
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/goal finish the bounded task\r");
    dsh.expect(b"goal round one finished");
    dsh.expect(b"goal is complete");
    let settled = dsh.checkpoint();
    dsh.write(b"/goal\r");
    dsh.expect_after(settled, b"complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 3);
    assert!(last_user_content(&requests[0]).contains("Round: 1/32"));
    assert!(last_user_content(&requests[0]).contains("finish the bounded task"));
    assert!(last_user_content(&requests[1]).contains("Round: 2/32"));
    let wrapup = last_user_content(&requests[2]);
    assert!(wrapup.contains("<goal_complete>"));
    assert!(wrapup.contains("finish the bounded task"));
    assert!(wrapup.contains("Do not call any more tools"));
    let first = request_json(&requests[0]);
    let names = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        names
            .windows(3)
            .any(|window| window == ["get_goal", "create_goal", "update_goal"])
    );
}

#[test]
fn cancelling_a_goal_round_pauses_automatic_continuation() {
    let partial =
        "data: {\"choices\":[{\"delta\":{\"content\":\"goal work started\"}}]}\n\n".to_owned();
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/goal keep working until cancelled\r");
    dsh.expect(b"goal work started");
    dsh.write(b"\x03");
    dsh.expect(b"stopped");
    dsh.expect("❯".as_bytes());
    let paused = dsh.checkpoint();
    dsh.write(b"/goal\r");
    dsh.expect_after(paused, b"paused");
    let (status, _) = dsh.exit_cleanly();
    let (request, closed) = server.finish();

    assert!(status.success());
    assert!(closed);
    assert!(last_user_content(&request).contains("Round: 1/32"));
}

#[test]
fn resumed_goal_is_restored_disarmed_and_requires_explicit_resume() {
    let partial =
        "data: {\"choices\":[{\"delta\":{\"content\":\"durable goal started\"}}]}\n\n".to_owned();
    let first_server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut first = PtyHarness::spawn_color_with_session_root_cargo(
        &first_server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    first.expect("❯".as_bytes());
    first.write(b"/goal persist across restart\r");
    first.expect(b"durable goal started");
    first.write(b"\x03");
    first.expect(b"stopped");
    first.expect("❯".as_bytes());
    let (status, _) = first.exit_cleanly();
    let (request, closed) = first_server.finish();
    assert!(status.success());
    assert!(closed);
    assert!(last_user_content(&request).contains("Round: 1/32"));

    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let filename = entries[0].file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();

    let resumed_server = DynamicGoalSseServer::start(Vec::new(), text_sse("resumed goal complete"));
    let caller_workspace = TestWorkspace::new();
    let mut resumed = PtyHarness::spawn_resume_color_cargo(
        &resumed_server.base_url,
        &caller_workspace.0,
        session_root.clone(),
        &session_id,
    );
    resumed.expect(format!("resumed session {session_id}").as_bytes());
    resumed.expect("❯".as_bytes());
    let shown = resumed.checkpoint();
    resumed.write(b"/goal\r");
    resumed.expect_after(shown, b"paused");
    resumed.expect_after(shown, b"disarmed");
    resumed.write(b"/goal resume\r");
    resumed.expect(b"resumed goal complete");
    let settled = resumed.checkpoint();
    resumed.write(b"/goal\r");
    resumed.expect_after(settled, b"complete");
    let (status, _) = resumed.exit_cleanly();
    let requests = resumed_server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(last_user_content(&requests[0]).contains("Round: 2/32"));
    assert!(last_user_content(&requests[1]).contains("<goal_complete>"));
    let journal = std::fs::read_to_string(session_root.path().join(filename)).unwrap();
    let event_types = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|row| {
            row.get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    assert!(
        event_types.windows(4).any(|window| {
            window == ["tool/call", "goal/change", "tool/result", "user/message"]
        })
    );
}

#[test]
fn active_turn_paste_fence_requires_a_fresh_enter_before_queueing() {
    let first =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"active paste busy\"}}]}\n\n")
            .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedThenStalledSseServer::start(first, finish);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start active paste fence\r");
    dsh.expect(b"active paste busy");
    dsh.write(b"\x1b[200~queued only after fresh enter\x1b[201~");
    dsh.expect(b"Paste inserted");
    let fence = dsh.checkpoint();
    dsh.write(b"\r");
    std::thread::sleep(Duration::from_millis(250));
    let snapshot = dsh.snapshot();
    assert!(
        !snapshot[fence..]
            .windows(b"next-turn prompt(s) queued".len())
            .any(|bytes| bytes == b"next-turn prompt(s) queued")
    );

    dsh.expect(b"Paste ready");
    dsh.write(b"\r");
    dsh.expect(b"next-turn prompt(s) queued");
    server.release();
    server.wait_until_second_request();
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    let (status, _) = dsh.exit_cleanly();
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        last_user_content(&requests[1]),
        "queued only after fresh enter"
    );
}

#[test]
fn an_in_flight_queue_front_cannot_be_recalled_or_replaced_by_the_next_draft() {
    let first = concat!("data: {\"choices\":[{\"delta\":{\"content\":\"first turn busy\"}}]}\n\n")
        .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedThenStalledSseServer::start(first, finish);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start the gated turn\r");
    dsh.expect(b"first turn busy");
    dsh.write(b"reserved prompt A\r");
    dsh.expect(b"next-turn prompt(s) queued");
    server.release();
    server.wait_until_second_request();

    dsh.write(b"draft C");
    dsh.expect(b"draft C");
    let history = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(history, b"start the gated turn");
    let recovery = dsh.checkpoint();
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    dsh.write(b"\x1b[B");
    dsh.write(b"\x1b[B");
    dsh.expect_after(recovery, b"draft C");
    dsh.write(&[0x15]);
    dsh.write(b"/exit\r");
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(last_user_content(&requests[0]), "start the gated turn");
    assert_eq!(last_user_content(&requests[1]), "reserved prompt A");
    assert!(!requests[1].contains("draft C"));
}

#[test]
fn a_reserved_auto_turn_is_settled_before_suspend_and_returns_as_history() {
    let first = concat!("data: {\"choices\":[{\"delta\":{\"content\":\"first turn busy\"}}]}\n\n")
        .to_owned();
    let finish = concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let mut server = GatedThenStalledSseServer::start(first, finish);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start before queue suspension\r");
    dsh.expect(b"first turn busy");
    dsh.write(b"queued prompt survives suspension\r");
    dsh.expect(b"next-turn prompt(s) queued");
    server.release();
    server.wait_until_second_request();

    dsh.signal(Signal::TSTP);
    dsh.wait_until_stopped();
    assert_eq!(dsh.terminal_state(), dsh.initial_terminal_state());
    let resumed = dsh.checkpoint();
    dsh.signal(Signal::CONT);
    dsh.expect_after(resumed, b"Ready");
    assert!(dsh.terminal_uses_application_mode());

    let history = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(history, b"queued prompt survives suspension");
    dsh.write(&[0x15]);
    dsh.write(b"/exit\r");
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let (requests, second_closed) = server.finish();

    assert!(status.success());
    assert!(second_closed);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        last_user_content(&requests[1]),
        "queued prompt survives suspension"
    );
}

#[test]
fn enhanced_resize_during_a_partial_stream_reanchors_without_cancelling_the_turn() {
    let partial_text = "busy-partial\n| RESIZE_TABLE | Value |\n";
    let partial = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
        serde_json::to_string(partial_text).unwrap()
    );
    let mut server = GatedFirstSseServer::start(
        partial,
        text_sse("| --- | --- |\n| body | continuation-after-resize |\n"),
        Vec::new(),
    );
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"resize the active stream\r");
    dsh.expect(b"busy-partial");
    let resize = dsh.checkpoint();
    dsh.resize(30, 80);
    dsh.expect_after(resize, b"Working | type the next prompt while dsh runs");
    assert!(
        !dsh.snapshot()
            .windows(b"RESIZE_TABLE".len())
            .any(|window| window == b"RESIZE_TABLE"),
        "a held table header must not leak before its delimiter arrives"
    );
    server.release();
    dsh.expect(b"\x1b[2;36m|\x1b[0m\x1b[1;36m RESIZE_TABLE");
    dsh.expect(b"continuation-after-resize");
    dsh.expect(b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(
        transcript
            .windows(b"RESIZE_TABLE".len())
            .filter(|window| *window == b"RESIZE_TABLE")
            .count(),
        1
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "resize the active stream");
}

#[test]
fn auto_edit_commits_a_workspace_patch_without_opening_the_approval_selector() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-auto-edit",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("auto edit finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh =
        PtyHarness::spawn_color_with_approval_mode(&server.base_url, &workspace.0, "auto-edit");

    dsh.expect("❯".as_bytes());
    let turn = dsh.checkpoint();
    dsh.write(b"apply the prepared edit automatically\r");
    dsh.expect_after(turn, b"Updated  note.txt");
    dsh.expect_after(turn, b"auto edit finished");
    dsh.expect_after(turn, b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert!(
        !transcript
            .windows(b"Proposed update".len())
            .any(|window| { window == b"Proposed update" })
    );
    assert!(
        !transcript
            .windows(b"> Reject".len())
            .any(|window| window == b"> Reject")
    );
    assert_eq!(requests.len(), 2);
}

#[test]
fn auto_edit_commits_a_literal_editor_change_without_opening_approval() {
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-auto-editor",
            "str_replace_editor",
            serde_json::json!({
                "command": "str_replace",
                "path": target.to_str().unwrap(),
                "old_str": "old",
                "new_str": "new"
            }),
        ),
        text_sse("literal editor finished"),
    ]);
    let mut dsh =
        PtyHarness::spawn_color_with_approval_mode(&server.base_url, &workspace.0, "auto-edit");

    dsh.expect("❯".as_bytes());
    let turn = dsh.checkpoint();
    dsh.write(b"replace the exact text automatically\r");
    dsh.expect_after(turn, b"Updated  note.txt");
    dsh.expect_after(turn, b"literal editor finished");
    dsh.expect_after(turn, b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert!(
        !transcript
            .windows(b"> Reject".len())
            .any(|row| row == b"> Reject")
    );
    assert_eq!(requests.len(), 2);
    let first = request_json(&requests[0]);
    let editor = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "str_replace_editor")
        .unwrap();
    assert_eq!(
        editor["function"]["parameters"]["properties"]["command"]["enum"],
        serde_json::json!(["view", "create", "str_replace", "insert"])
    );
}

#[test]
fn literal_editor_uses_the_normal_semantic_diff_approval_when_ask_is_active() {
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-ask-editor",
            "str_replace_editor",
            serde_json::json!({
                "command": "str_replace",
                "path": target.to_str().unwrap(),
                "old_str": "old",
                "new_str": "new"
            }),
        ),
        text_sse("approved literal editor finished"),
    ]);
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"show and approve the exact edit\r");
    dsh.approval_ready();
    dsh.expect(b"Proposed update");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"\x1b[A");
    dsh.expect(b"> Allow once");
    dsh.write(b"\r");
    dsh.expect(b"Updated  note.txt");
    dsh.expect(b"approved literal editor finished");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn official_edit_replace_all_uses_the_normal_semantic_diff_approval() {
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old and old\n").expect("test file should be created");
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-edit-all",
            "edit",
            serde_json::json!({
                "file_path": "note.txt",
                "old_string": "old",
                "new_string": "new",
                "replace_all": true
            }),
        ),
        text_sse("approved edit finished"),
    ]);
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"replace every exact match\r");
    dsh.approval_ready();
    dsh.expect(b"Proposed update");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old and old\n");
    dsh.write(b"\x1b[A");
    dsh.expect(b"> Allow once");
    dsh.write(b"\r");
    dsh.expect(b"Updated  note.txt");
    dsh.expect(b"approved edit finished");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new and new\n");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-edit-all"),
        "The file note.txt has been updated. All occurrences were successfully replaced."
    );
    let tools = request_json(&requests[0])["tools"]
        .as_array()
        .unwrap()
        .clone();
    for name in ["write", "edit"] {
        assert!(tools.iter().any(|tool| tool["function"]["name"] == name));
    }
}

#[test]
fn user_question_selection_returns_the_displayed_label_and_continues_the_turn() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-mode",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id": "mode",
                    "header": "Choose mode",
                    "question": "Which verification mode should I use?",
                    "options": [
                        {
                            "label": "Thorough (Recommended)",
                            "description": "Run the full local suite."
                        },
                        {
                            "label": "Focused",
                            "description": "Run only the necessary checks."
                        }
                    ]
                }]
            }),
        ),
        text_sse("I will use focused verification."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask me before choosing the verification mode\r");
    dsh.expect(b"Which verification mode should I use?");
    dsh.expect(b"2. Focused");
    dsh.expect(b"Press 1-2 to choose");
    dsh.write(b"2");
    dsh.expect(b"I will use focused verification.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-mode"),
        r#"{"answers":[{"id":"mode","selected":["Focused"]}]}"#
    );
    assert!(
        request_json(&requests[0])["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "ask_user_question")
    );
}

#[test]
fn user_question_escape_returns_a_cancelled_tool_result_without_choosing() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-cancel",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id": "release",
                    "question": "Ship this release?",
                    "options": [{"label":"Ship"},{"label":"Wait"}]
                }]
            }),
        ),
        text_sse("The question was cancelled, so I did not choose."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask whether to ship\r");
    dsh.expect(b"Ship this release?");
    dsh.expect(b"Press 1-2 to choose");
    dsh.write(&[0x1b]);
    dsh.expect(b"The question was cancelled, so I did not choose.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-cancel"),
        "Error: ask_user_question was cancelled before the user answered"
    );
}

#[test]
fn user_question_batch_collects_each_choice_before_continuing() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-batch",
            "ask_user_question",
            serde_json::json!({
                "questions": [
                    {
                        "id": "mode",
                        "question": "Which implementation mode?",
                        "options": [{"label":"Safe"},{"label":"Fast"}]
                    },
                    {
                        "id": "tests",
                        "question": "Which validation scope?",
                        "options": [{"label":"Focused"},{"label":"Full"}]
                    }
                ]
            }),
        ),
        text_sse("I have both decisions and can continue."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for both decisions\r");
    dsh.expect(b"question 1/2 from assistant");
    dsh.expect(b"Which implementation mode?");
    dsh.write(b"2");
    dsh.expect(b"question 2/2 from assistant");
    dsh.expect(b"Which validation scope?");
    dsh.write(b"1");
    dsh.expect(b"I have both decisions and can continue.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-batch"),
        concat!(
            r#"{"answers":[{"id":"mode","selected":["Fast"]},"#,
            r#"{"id":"tests","selected":["Focused"]}]}"#,
        )
    );
}

#[test]
fn user_question_optionless_custom_answer_continues_with_exact_unicode_text() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-custom",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id": "detail",
                    "question": "How should I validate this change?"
                }]
            }),
        ),
        text_sse("I will follow the custom validation scope."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for a custom validation scope\r");
    dsh.expect(b"How should I validate this change?");
    dsh.expect(b"Enter answer | Ctrl+J newline");
    dsh.write(" 只跑必要检查".as_bytes());
    dsh.write(&[0x0a]);
    dsh.write("并保留草稿 \r".as_bytes());
    dsh.expect(b"I will follow the custom validation scope.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-custom"),
        r#"{"answers":[{"custom":"只跑必要检查\n并保留草稿","id":"detail","selected":[]}]}"#
    );
}

#[test]
fn user_question_custom_escape_cancels_without_publishing_partial_text() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-custom-cancel",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id": "detail",
                    "question": "Describe the optional change."
                }]
            }),
        ),
        text_sse("The custom question was cancelled."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for optional details\r");
    dsh.expect(b"Describe the optional change.");
    dsh.expect(b"Enter answer | Ctrl+J newline");
    dsh.write(b"partial text");
    dsh.write(&[0x1b]);
    dsh.expect(b"The custom question was cancelled.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-custom-cancel"),
        "Error: ask_user_question was cancelled before the user answered"
    );
}

#[test]
fn user_question_mixed_batch_keeps_choice_and_custom_answers_in_order() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-mixed",
            "ask_user_question",
            serde_json::json!({
                "questions": [
                    {
                        "id": "mode",
                        "question": "Which implementation mode?",
                        "options": [{"label":"Safe"},{"label":"Fast"}]
                    },
                    {
                        "id": "detail",
                        "question": "What local check should I run?",
                        "options": [{"label":"Format"},{"label":"Unit tests"}]
                    }
                ]
            }),
        ),
        text_sse("I received the mixed answers."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for a choice and a custom check\r");
    dsh.expect(b"question 1/2 from assistant");
    dsh.write(b"1");
    dsh.expect(b"question 2/2 from assistant");
    dsh.expect(b"3. Other (type your own answer)");
    dsh.write(b"3");
    dsh.expect(b"Enter answer | Ctrl+J newline");
    dsh.write(b"cargo test focused\r");
    dsh.expect(b"I received the mixed answers.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-mixed"),
        concat!(
            r#"{"answers":[{"id":"mode","selected":["Safe"]},"#,
            r#"{"custom":"cargo test focused","id":"detail","selected":[]}]}"#,
        )
    );
}

#[test]
fn user_question_multi_select_toggles_and_returns_current_user_order() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-multi",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id":"targets",
                    "question":"What should I update?",
                    "options":[
                        {"label":"tests"},
                        {"label":"docs"},
                        {"label":"examples"}
                    ],
                    "multi_select":true
                }]
            }),
        ),
        text_sse("I received the selected targets."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask which targets to update\r");
    dsh.expect(b"Press 1-3 to toggle");
    dsh.expect(b"No options selected");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 1");
    dsh.write(b"2");
    dsh.expect(b"Selected options \xc2\xb7 1,2");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 2");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 1,2");
    dsh.write(b"\r");
    dsh.expect(b"I received the selected targets.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-multi"),
        r#"{"answers":[{"id":"targets","selected":["docs","tests"]}]}"#
    );
}

#[test]
fn user_question_multi_select_custom_supplements_selected_labels() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-multi-custom",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id":"targets",
                    "question":"What should I update?",
                    "options":[{"label":"tests"},{"label":"docs"}],
                    "multi_select":true
                }]
            }),
        ),
        text_sse("I received labels and custom text."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for targets and a custom addition\r");
    dsh.expect(b"Press 1-2 to toggle");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 1");
    dsh.write(b"3");
    dsh.expect(b"Enter answer | Ctrl+J newline");
    dsh.write(b"release notes\r");
    dsh.expect(b"I received labels and custom text.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-multi-custom"),
        r#"{"answers":[{"custom":"release notes","id":"targets","selected":["tests"]}]}"#
    );
}

#[test]
fn user_question_multi_select_escape_cancels_the_whole_question() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-multi-cancel",
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id":"targets",
                    "question":"What should I update?",
                    "options":[{"label":"tests"},{"label":"docs"}],
                    "multi_select":true
                }]
            }),
        ),
        text_sse("The multi-select question was cancelled."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask for targets then cancel\r");
    dsh.expect(b"Press 1-2 to toggle");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 1");
    dsh.write(&[0x1b]);
    dsh.expect(b"The multi-select question was cancelled.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-multi-cancel"),
        "Error: ask_user_question was cancelled before the user answered"
    );
}

#[test]
fn user_question_skip_custom_middle_keeps_earlier_and_later_answers() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-skip-middle",
            "ask_user_question",
            serde_json::json!({
                "questions":[
                    {"id":"mode","question":"Which mode?","options":[{"label":"Safe"},{"label":"Fast"}]},
                    {"id":"detail","question":"Anything else?"},
                    {"id":"tests","question":"Which tests?","options":[{"label":"Focused"},{"label":"Full"}]}
                ]
            }),
        ),
        text_sse("I received the answers around the skipped question."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask three questions and let me skip one\r");
    dsh.expect(b"question 1/3 from assistant");
    dsh.write(b"2");
    dsh.expect(b"question 2/3 from assistant");
    dsh.expect(b"Ctrl+S skips");
    dsh.write(&[0x13]);
    dsh.expect(b"question 3/3 from assistant");
    dsh.write(b"1");
    dsh.expect(b"I received the answers around the skipped question.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-skip-middle"),
        concat!(
            r#"{"answers":[{"id":"mode","selected":["Fast"]},"#,
            r#"{"id":"detail","selected":[]},"#,
            r#"{"id":"tests","selected":["Focused"]}]}"#,
        )
    );
}

#[test]
fn user_question_skip_final_multi_discards_its_partial_selection() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-skip-multi",
            "ask_user_question",
            serde_json::json!({
                "questions":[{
                    "id":"targets",
                    "question":"Which targets?",
                    "options":[{"label":"tests"},{"label":"docs"}],
                    "multi_select":true
                }]
            }),
        ),
        text_sse("I saw the skipped multi-select question."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask a multi question then skip it\r");
    dsh.expect(b"Press 1-2 to toggle");
    dsh.write(b"1");
    dsh.expect(b"Selected options \xc2\xb7 1");
    dsh.write(b"s");
    dsh.expect(b"I saw the skipped multi-select question.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-skip-multi"),
        r#"{"answers":[{"id":"targets","selected":[]}]}"#
    );
}

#[test]
fn user_question_pager_retains_drafts_and_returns_to_the_missing_first_question() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-question-pager",
            "ask_user_question",
            serde_json::json!({
                "questions":[
                    {"id":"mode","question":"Which mode?","options":[{"label":"Safe"},{"label":"Fast"}]},
                    {"id":"detail","question":"What local detail should I keep?"},
                    {
                        "id":"targets",
                        "question":"Which targets?",
                        "options":[{"label":"tests"},{"label":"docs"}],
                        "multi_select":true
                    }
                ]
            }),
        ),
        text_sse("I received the completed paged answers."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"ask three paged questions\r");
    dsh.expect(b"question 1/3 from assistant");
    dsh.write(b"]");
    dsh.expect(b"question 2/3 from assistant");
    dsh.write("必要检查".as_bytes());
    dsh.write(&[0x0e]);
    dsh.expect(b"question 3/3 from assistant");
    dsh.write(b"2");
    dsh.expect(b"Selected options \xc2\xb7 2");
    let return_to_detail = dsh.checkpoint();
    dsh.write(b"[");
    dsh.expect_after(return_to_detail, b"question 2/3 from assistant");
    let return_to_mode = dsh.checkpoint();
    dsh.write(&[0x10]);
    dsh.expect_after(return_to_mode, b"question 1/3 from assistant");
    dsh.expect_after(return_to_mode, b"Press 1-2 to choose");
    let revisit_detail = dsh.checkpoint();
    dsh.write(b"2");
    dsh.expect_after(revisit_detail, b"question 2/3 from assistant");
    dsh.expect_after(revisit_detail, "必要检查".as_bytes());
    let revisit_targets = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(revisit_targets, b"question 3/3 from assistant");
    dsh.expect_after(revisit_targets, b"Selected options \xc2\xb7 2");
    dsh.write(b"\r");
    dsh.expect(b"I received the completed paged answers.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-question-pager"),
        concat!(
            r#"{"answers":[{"id":"mode","selected":["Fast"]},"#,
            r#"{"custom":"必要检查","id":"detail","selected":[]},"#,
            r#"{"id":"targets","selected":["docs"]}]}"#,
        )
    );
}

#[test]
fn plan_mode_reviews_the_exact_plan_and_exits_before_the_next_model_step() {
    let plan = "# Safe change\n\n- Inspect the project\n- Run focused tests";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-exit-plan",
            "exit_plan_mode",
            serde_json::json!({ "plan": plan }),
        ),
        text_sse("The approved plan is ready to implement."),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"/plan inspect the project first\r");
    dsh.expect(b"Plan Mode on");
    dsh.expect(b"# Safe change");
    dsh.expect(b"Run focused tests");
    dsh.expect(b"Press 1 to approve");
    dsh.write(b"1");
    dsh.expect(b"The approved plan is ready to implement.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(last_user_content(&requests[0]), "inspect the project first");
    assert!(system_message_content(&requests[0]).contains("You are in Plan Mode."));
    assert!(!system_message_content(&requests[1]).contains("You are in Plan Mode."));
    assert!(
        request_json(&requests[0])["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "exit_plan_mode")
    );
    assert_eq!(
        tool_message_content(&requests[1], "call-exit-plan"),
        "Plan approved — Plan Mode exited; carry out the plan starting with your next step."
    );
    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let journal = std::fs::read_to_string(entry.path()).unwrap();
    let rows = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    let tool_result = rows
        .iter()
        .position(|row| row["type"] == "tool/result")
        .unwrap();
    let exit = rows
        .iter()
        .position(|row| row["type"] == "plan/mode" && row["data"]["active"] == false)
        .unwrap();
    let changed_header = rows
        .iter()
        .enumerate()
        .skip(exit + 1)
        .find(|(_, row)| row["type"] == "request/header")
        .map(|(index, _)| index)
        .unwrap();
    assert!(tool_result < exit && exit < changed_header);
}

#[test]
fn plan_mode_feedback_stays_active_until_manual_off() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-plan-feedback",
            "exit_plan_mode",
            serde_json::json!({ "plan": "# First draft\n\nImplement it." }),
        ),
        text_sse("I will revise the plan with that feedback."),
        text_sse("Now I can implement outside Plan Mode."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/plan draft safely\r");
    dsh.expect(b"Press 1 to approve");
    dsh.write(b"3");
    dsh.expect(b"Enter answer | Ctrl+J newline");
    dsh.write(b"add resume tests\r");
    dsh.expect(b"I will revise the plan with that feedback.");
    dsh.expect(b"Turn complete");
    dsh.write(b"/plan off\r");
    dsh.expect(b"Plan Mode off");
    dsh.write(b"now implement\r");
    dsh.expect(b"Now I can implement outside Plan Mode.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 3);
    assert!(system_message_content(&requests[0]).contains("You are in Plan Mode."));
    assert!(system_message_content(&requests[1]).contains("You are in Plan Mode."));
    assert!(!system_message_content(&requests[2]).contains("You are in Plan Mode."));
    assert_eq!(
        tool_message_content(&requests[1], "call-plan-feedback"),
        "Error: The user chose to keep planning; their feedback: add resume tests"
    );
}

#[test]
fn dismissing_plan_review_keeps_plan_mode_for_the_model() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-plan-discuss",
            "exit_plan_mode",
            serde_json::json!({ "plan": "# Discuss first\n\nWait for the user." }),
        ),
        text_sse("I will wait for your message in Plan Mode."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/plan ask before acting\r");
    dsh.expect(b"Press 1 to approve");
    dsh.write(&[0x1b]);
    dsh.expect(b"I will wait for your message in Plan Mode.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(system_message_content(&requests[1]).contains("You are in Plan Mode."));
    assert_eq!(
        tool_message_content(&requests[1], "call-plan-discuss"),
        concat!(
            "Error: The user dismissed the plan review to speak instead; ",
            "stay in Plan Mode, stop here, and wait for their message."
        )
    );
}

#[test]
fn linear_plan_command_enters_sends_and_manually_exits_without_escape_bytes() {
    let server = SequenceSseServer::start(vec![text_sse("Linear planning response.")]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"/plan inspect in linear mode\r");
    dsh.expect(b"[Plan Mode on. Use /plan off to leave.]");
    dsh.expect(b"assistant | Linear planning response.");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 3);
    dsh.write(b"/plan off\r");
    dsh.expect(b"[Plan Mode off.]");
    dsh.expect_occurrences(b"dsh > ", 4);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert_eq!(requests.len(), 1);
    assert_eq!(last_user_content(&requests[0]), "inspect in linear mode");
    assert!(system_message_content(&requests[0]).contains("You are in Plan Mode."));
}

#[test]
fn todo_write_updates_the_enhanced_standing_plan_in_durable_order() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-todo",
            "todo_write",
            serde_json::json!({
                "todos": [
                    { "content": "inspect code", "status": "in_progress" },
                    { "content": "run focused tests", "status": "pending" }
                ]
            }),
        ),
        text_sse("The task list is recorded."),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"track this work\r");
    dsh.expect(b"Tasks  1 in progress \xc2\xb7 1 pending");
    dsh.expect(b"inspect code");
    dsh.expect(b"The task list is recorded.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_message_content(&requests[1], "call-todo"),
        "Updated todo list: 1 pending, 1 in progress, 0 completed."
    );
    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let journal = std::fs::read_to_string(entry.path()).unwrap();
    let event_types = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|row| row["type"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        event_types
            .windows(3)
            .any(|window| { window == ["tool/call", "todo/write", "tool/result"] })
    );
}

#[test]
fn todo_write_prints_the_complete_bounded_list_in_zero_escape_linear_mode() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-todo-linear",
            "todo_write",
            serde_json::json!({
                "todos": [
                    { "content": "inspect code", "status": "completed" },
                    { "content": "write fix", "status": "in_progress" },
                    { "content": "run checks", "status": "pending" }
                ]
            }),
        ),
        text_sse("Linear Todo finished."),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"show tasks in linear mode\r");
    dsh.expect(b"[tasks updated]");
    dsh.expect(b"[x] inspect code");
    dsh.expect(b"[~] write fix");
    dsh.expect(b"[ ] run checks");
    dsh.expect(b"assistant | Linear Todo finished.");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert!(!transcript.contains(&0x1b));
    assert_eq!(requests.len(), 2);
}

#[test]
fn manual_compact_runs_one_idle_request_and_persists_a_null_turn_transaction() {
    let server = SequenceSseServer::start(vec![
        text_sse(&"older assistant work ".repeat(240)),
        text_sse("preserve the earlier request and completed work"),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    let prompt = format!("remember this older requirement {}", "x".repeat(900));
    dsh.write(prompt.as_bytes());
    dsh.write(b"\r");
    dsh.expect(b"Turn complete");
    dsh.write(b"/compact\r");
    dsh.expect(b"Compacted 1 history items (~");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(last_user_content(&requests[1]).contains("Summarize the selected older"));

    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let rows = std::fs::read_to_string(entry.path())
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    let start = rows
        .iter()
        .position(|row| row["type"] == "compaction/start")
        .unwrap();
    assert_eq!(
        rows[start..start + 4]
            .iter()
            .map(|row| row["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "compaction/start",
            "compaction/summary",
            "user/message",
            "compaction/end",
        ]
    );
    assert!(rows[start]["data"]["turn"].is_null());
    assert_eq!(rows[start]["data"]["dispatch"]["trigger"], "manual");
    assert!(rows[start + 3]["data"]["turn"].is_null());
    assert_eq!(
        rows[start]["data"]["sourceCommandId"],
        rows[start + 3]["data"]["sourceCommandId"]
    );
}

#[test]
fn workspace_instructions_persist_reconcile_and_do_not_duplicate_on_resume() {
    let first_server = SequenceSseServer::start(vec![text_sse("Workspace guidance received.")]);
    let workspace = TestWorkspace::new();
    std::fs::write(
        workspace.0.join("AGENTS.md"),
        "Use the focused workspace instruction fixture.",
    )
    .unwrap();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &first_server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"inspect the instruction order\r");
    dsh.expect(b"Workspace guidance received.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = first_server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    let users = user_contents(&requests[0]);
    assert_eq!(users.len(), 2);
    assert_eq!(users[0], "inspect the instruction order");
    assert!(users[1].contains("Instructions from: AGENTS.md"));
    assert!(users[1].contains("Use the focused workspace instruction fixture."));
    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let filename = entries[0].file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();

    std::fs::write(
        workspace.0.join("AGENTS.md"),
        "Use the replacement workspace instruction fixture.",
    )
    .unwrap();
    let second_server = SequenceSseServer::start(vec![text_sse("Replacement received.")]);
    let mut resumed = PtyHarness::spawn_resume_color_cargo(
        &second_server.base_url,
        &workspace.0,
        session_root.clone(),
        &session_id,
    );
    resumed.expect("❯".as_bytes());
    resumed.write(b"reconcile changed instructions\r");
    resumed.expect(b"Replacement received.");
    resumed.expect(b"Turn complete");
    let (status, _) = resumed.exit_cleanly();
    let requests = second_server.finish();
    assert!(status.success());
    let users = user_contents(&requests[0]);
    assert_eq!(users[users.len() - 2], "reconcile changed instructions");
    assert!(
        users
            .last()
            .unwrap()
            .contains("Updated instructions from: AGENTS.md")
    );
    assert!(
        users
            .last()
            .unwrap()
            .contains("Use the replacement workspace instruction fixture.")
    );

    let third_server = SequenceSseServer::start(vec![text_sse("No duplicate received.")]);
    let mut unchanged = PtyHarness::spawn_resume_color_cargo(
        &third_server.base_url,
        &workspace.0,
        session_root.clone(),
        &session_id,
    );
    unchanged.expect("❯".as_bytes());
    unchanged.write(b"reuse unchanged instructions\r");
    unchanged.expect(b"No duplicate received.");
    unchanged.expect(b"Turn complete");
    let (status, _) = unchanged.exit_cleanly();
    let requests = third_server.finish();
    assert!(status.success());
    assert_eq!(
        user_contents(&requests[0])
            .iter()
            .filter(|content| content.contains("<system-reminder>"))
            .count(),
        2
    );

    let rows = std::fs::read_to_string(session_root.path().join(filename))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    let user_rows = rows
        .iter()
        .filter(|row| row["type"] == "user/message")
        .collect::<Vec<_>>();
    assert_eq!(user_rows.len(), 5);
    assert_eq!(user_rows[0]["data"]["source"]["kind"], "user");
    assert_eq!(user_rows[1]["data"]["source"]["kind"], "agent-instructions");
    assert_eq!(user_rows[1]["data"]["source"]["baseline"], true);
    assert_eq!(user_rows[3]["data"]["source"]["kind"], "agent-instructions");
    assert_eq!(
        user_rows[3]["data"]["source"]["changes"][0]["action"],
        "replace"
    );
}

#[test]
fn successful_read_injects_nested_workspace_instructions_into_the_next_real_request() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-read-nested-instructions",
            "read",
            serde_json::json!({ "file_path": "pkg/deep/file.txt" }),
        ),
        text_sse("Nested guidance applied."),
    ]);
    let workspace = TestWorkspace::new();
    std::fs::create_dir_all(workspace.0.join("pkg/deep")).unwrap();
    std::fs::write(
        workspace.0.join("AGENTS.md"),
        "Use the root dynamic instruction fixture.",
    )
    .unwrap();
    std::fs::write(
        workspace.0.join("pkg/AGENTS.md"),
        "Use the nested dynamic instruction fixture.",
    )
    .unwrap();
    std::fs::write(workspace.0.join("pkg/deep/file.txt"), "hello\n").unwrap();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"read the nested fixture and continue\r");
    dsh.expect(b"Nested guidance applied.");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(
        tool_message_content(&requests[1], "call-read-nested-instructions")
            .contains("pkg/deep/file.txt")
    );
    assert!(user_contents(&requests[1]).iter().any(|content| {
        content.contains("Additional instructions from: pkg/AGENTS.md")
            && content.contains("Use the nested dynamic instruction fixture.")
    }));

    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let rows = std::fs::read_to_string(entry.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let result_index = rows
        .iter()
        .position(|row| row["type"] == "tool/result")
        .unwrap();
    let first_step_end = rows
        .iter()
        .position(|row| row["type"] == "step/end")
        .unwrap();
    let nested_context = rows
        .iter()
        .position(|row| {
            row["type"] == "user/message"
                && row["data"]["source"]["kind"] == "agent-instructions"
                && row["data"]["source"]["changes"]
                    .as_array()
                    .is_some_and(|changes| {
                        changes
                            .iter()
                            .any(|change| change["path"] == "pkg/AGENTS.md")
                    })
        })
        .unwrap();
    assert!(result_index < first_step_end && first_step_end < nested_context);
    assert!(
        rows[first_step_end + 1..nested_context]
            .iter()
            .any(|row| row["type"] == "step/start")
    );
}

#[test]
fn project_skill_catalog_and_loader_reach_the_real_enhanced_cli_without_approval() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-load-project-skill",
            "skill",
            serde_json::json!({ "name": "demo-skill" }),
        ),
        text_sse("Project Skill applied."),
    ]);
    let workspace = TestWorkspace::new();
    let skill_dir = workspace.0.join(".dsh/skills/demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: Use the project demo safely.\n---\nFollow the real project Skill body.\n",
    )
    .unwrap();
    let session_root = TestSessionRoot::new();
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"use the matching project skill\r");
    dsh.expect(b"Project Skill applied.");
    dsh.expect(b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    let first = request_json(&requests[0]);
    let schema = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "skill")
        .unwrap();
    assert_eq!(
        schema["function"]["parameters"]["required"],
        serde_json::json!(["name"])
    );
    assert_eq!(
        schema["function"]["parameters"]["additionalProperties"],
        false
    );
    let users = user_contents(&requests[0]);
    assert_eq!(users[0], "use the matching project skill");
    assert!(users[1].contains("<available_skills>"));
    assert!(users[1].contains("- `demo-skill`: Use the project demo safely."));
    let result = tool_message_content(&requests[1], "call-load-project-skill");
    assert!(result.contains("<skill_content name=\"demo-skill\">"));
    assert!(result.contains("Follow the real project Skill body."));
    assert!(result.contains(".dsh/skills/demo-skill"));
    assert!(
        !transcript
            .windows(b"> Reject".len())
            .any(|row| row == b"> Reject")
    );

    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let rows = std::fs::read_to_string(entry.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let catalog = rows
        .iter()
        .position(|row| {
            row["type"] == "user/message" && row["data"]["source"]["kind"] == "skill-catalog"
        })
        .unwrap();
    let call = rows
        .iter()
        .position(|row| row["type"] == "tool/call")
        .unwrap();
    let result = rows
        .iter()
        .position(|row| row["type"] == "tool/result")
        .unwrap();
    assert!(catalog < call && call < result);
}

#[test]
fn resumed_project_skill_catalog_appends_a_complete_replacement_before_provider_work() {
    let workspace = TestWorkspace::new();
    let skill_dir = workspace.0.join(".agents/skills/resume-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_file = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_file,
        "---\nname: resume-skill\ndescription: First catalog\n---\nFirst body.\n",
    )
    .unwrap();
    let session_root = TestSessionRoot::new();
    let first_server = SequenceSseServer::start(vec![text_sse("First catalog received.")]);
    let mut first = PtyHarness::spawn_color_with_session_root_cargo(
        &first_server.base_url,
        &workspace.0,
        session_root.clone(),
    );
    first.expect("❯".as_bytes());
    first.write(b"record the first skill catalog\r");
    first.expect(b"First catalog received.");
    first.expect(b"Turn complete");
    let (status, _) = first.exit_cleanly();
    assert!(status.success());
    assert_eq!(first_server.finish().len(), 1);

    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let filename = entry.file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();
    std::fs::write(
        &skill_file,
        "---\nname: resume-skill\ndescription: Replacement catalog\n---\nSecond body.\n",
    )
    .unwrap();

    let second_server = SequenceSseServer::start(vec![text_sse("Replacement received.")]);
    let mut resumed = PtyHarness::spawn_resume_color_cargo(
        &second_server.base_url,
        &workspace.0,
        session_root.clone(),
        &session_id,
    );
    resumed.expect("❯".as_bytes());
    resumed.write(b"observe the updated skill catalog\r");
    resumed.expect(b"Replacement received.");
    resumed.expect(b"Turn complete");
    let (status, _) = resumed.exit_cleanly();
    let requests = second_server.finish();
    assert!(status.success());
    assert_eq!(requests.len(), 1);
    let users = user_contents(&requests[0]);
    assert!(users.iter().any(|text| text.contains("First catalog")));
    assert!(users.last().unwrap().contains("Replacement catalog"));
    assert!(users.last().unwrap().contains("complete catalog replaces"));

    let rows = std::fs::read_to_string(session_root.path().join(filename))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let catalogs = rows
        .iter()
        .filter(|row| {
            row["type"] == "user/message" && row["data"]["source"]["kind"] == "skill-catalog"
        })
        .collect::<Vec<_>>();
    assert_eq!(catalogs.len(), 2);
    assert_eq!(catalogs[1]["data"]["source"]["update"], true);
    assert_eq!(
        catalogs[1]["data"]["source"]["entries"][0]["description"],
        "Replacement catalog"
    );
}

#[test]
fn auto_edit_is_not_persisted_and_resume_returns_to_ask() {
    let first_patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+middle\n";
    let first_server = SequenceSseServer::start(vec![
        tool_sse(
            "call-auto-edit-seed",
            "apply_patch",
            serde_json::json!({ "patch": first_patch }),
        ),
        text_sse("auto-edit seed finished"),
    ]);
    let workspace = TestWorkspace::new();
    let caller_workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").unwrap();
    let mut first = PtyHarness::spawn_color_with_approval_mode_and_session_root(
        &first_server.base_url,
        &workspace.0,
        "auto-edit",
        session_root.clone(),
    );

    first.expect("❯".as_bytes());
    first.write(b"seed an auto-edit session\r");
    first.expect(b"Updated  note.txt");
    first.expect(b"auto-edit seed finished");
    first.expect(b"Turn complete");
    let (status, transcript) = first.exit_cleanly();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "middle\n");
    assert!(
        !transcript
            .windows(b"> Reject".len())
            .any(|row| row == b"> Reject")
    );
    assert_eq!(first_server.finish().len(), 2);

    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let filename = entries[0].file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();
    let journal = std::fs::read(entries[0].path()).unwrap();
    for event in [
        b"\"type\":\"approval/asked\"".as_slice(),
        b"\"type\":\"approval/decided\"".as_slice(),
    ] {
        assert!(!journal.windows(event.len()).any(|row| row == event));
    }

    let second_patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-middle\n+new\n";
    let second_server = SequenceSseServer::start(vec![
        tool_sse(
            "call-resumed-ask",
            "apply_patch",
            serde_json::json!({ "patch": second_patch }),
        ),
        text_sse("resume ask restored"),
    ]);
    let mut resumed = PtyHarness::spawn_resume_color_cargo(
        &second_server.base_url,
        &caller_workspace.0,
        session_root,
        &session_id,
    );
    resumed.expect("❯".as_bytes());
    resumed.write(b"prove approval mode reset\r");
    resumed.approval_ready();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "middle\n");
    resumed.write(b"\r");
    resumed.expect(b"Rejected");
    resumed.expect(b"resume ask restored");
    resumed.expect(b"Turn complete");
    let (status, _) = resumed.exit_cleanly();
    let requests = second_server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "middle\n");
    assert_eq!(requests.len(), 2);
}

#[test]
fn styled_approval_selector_is_visible_safe_and_restores_the_terminal() {
    let patch = concat!(
        "--- a/note.txt\n",
        "+++ b/note.txt\n",
        "@@ -1,2 +1,2 @@\n",
        "--- a/decoy\n",
        "-++ b/decoy\n",
        "+-- A/DECOY\n",
        "+++ B/DECOY\n",
    );
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-styled-patch",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("styled patch finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    let old = "-- a/decoy\n++ b/decoy\n";
    let new = "-- A/DECOY\n++ B/DECOY\n";
    std::fs::write(&target, old).expect("test file should be created");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/theme paper\r");
    dsh.expect("Theme changed · paper".as_bytes());
    dsh.write(b"show the styled approval selector\r");
    dsh.expect(b"Proposed update");
    dsh.expect(b"note.txt");
    dsh.expect(b"+2 -2");
    dsh.expect(b"\x1b[1;38;5;25m--- a/note.txt");
    dsh.expect(b"\x1b[38;5;24m@@ -1,2 +1,2 @@");
    dsh.expect(b"\x1b[1;38;5;124m--- a/decoy");
    dsh.expect(b"\x1b[38;5;28m+++ B/DECOY");
    dsh.approval_ready();
    dsh.expect(b"> Reject");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), old);
    let compact = dsh.checkpoint();
    dsh.resize(5, 12);
    dsh.expect_after(compact, b"Not applied");
    dsh.expect_after(compact, b"> Reject");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), old);
    let selection = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(selection, b"> Allow...");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), old);
    dsh.write(b"\r");
    dsh.expect(b"Updated  note.txt");
    dsh.expect(b"styled patch finished");
    dsh.expect_occurrences("❯".as_bytes(), 2);
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), new);
    assert!(transcript.contains(&0x1b));
    for sentinel in [b"--- a/decoy".as_slice(), b"+++ B/DECOY"] {
        assert_eq!(
            transcript
                .windows(sentinel.len())
                .filter(|window| *window == sentinel)
                .count(),
            1,
            "the immutable approval preview must enter scrollback once"
        );
    }
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn enhanced_approval_takes_over_inspect_before_rendering_the_preview() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"waiting before approval\"}}]}\n\n")
            .to_owned();
    let mut server = GatedFirstSseServer::start(
        partial,
        tool_sse(
            "call-inspect-takeover",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        vec![text_sse("rejected after inspect takeover")],
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"/theme paper\r");
    dsh.expect("Theme changed · paper".as_bytes());
    dsh.write(b"open inspect before approval\r");
    dsh.expect(b"waiting before approval");
    let inspect = dsh.checkpoint();
    dsh.write(&[0x0f]);
    dsh.expect_after(inspect, b"INSPECT");
    dsh.write(b"\x1b[6~\r");
    dsh.write(b"\x1b[200~\x1b[C\r\x1b[201~");

    let takeover = dsh.checkpoint();
    server.release();
    dsh.expect_after(takeover, b"Requested  Patch");
    dsh.expect_after(takeover, b"Proposed update");
    dsh.expect_after(takeover, b"\x1b[1;38;5;25m--- a/note.txt");
    dsh.expect_after(takeover, b"\x1b[1;38;5;124m-old");
    dsh.expect_after(takeover, b"\x1b[38;5;28m+new");
    dsh.approval_ready();
    dsh.expect(b"> Reject");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"\r");
    dsh.expect(b"Rejected");
    dsh.expect(b"rejected after inspect takeover");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 2);
}

#[test]
fn enhanced_approval_suppresses_then_restores_the_command_palette_draft() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let partial = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"waiting with palette draft\"}}]}\n\n"
    )
    .to_owned();
    let mut server = GatedFirstSseServer::start(
        partial,
        tool_sse(
            "call-palette-takeover",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        vec![text_sse("rejected after palette takeover")],
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"open approval over palette\r");
    dsh.expect(b"waiting with palette draft");
    let palette = dsh.checkpoint();
    dsh.write(b"/he");
    dsh.expect_after(palette, b"> /help");

    let takeover = dsh.checkpoint();
    server.release();
    dsh.expect_after(takeover, b"Requested  Patch");
    dsh.expect_after(takeover, b"Proposed update");
    dsh.write(b"\x15/exit\r");
    let approval_dock = dsh.checkpoint();
    dsh.approval_ready();
    dsh.expect_after(approval_dock, b"> Reject");
    assert!(
        !dsh.snapshot()[approval_dock..]
            .windows(b"> /help".len())
            .any(|window| window == b"> /help"),
        "approval owns the Dock and must suppress the command palette"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    let settlement = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(settlement, b"Rejected");
    dsh.expect_after(settlement, b"rejected after palette takeover");
    dsh.expect_after(settlement, b"Turn complete");
    dsh.expect_after(settlement, b"Ready");
    dsh.expect_after(settlement, b"> /help");
    dsh.write(b"\x15/exit\r");
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 2);
}

#[test]
fn enhanced_approval_discards_stale_file_menu_input_and_rescans_after_reject() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"waiting with file draft\"}}]}\n\n"
        .to_owned();
    let mut server = GatedFirstSseServer::start(
        partial,
        tool_sse(
            "call-file-takeover",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        vec![text_sse("rejected after file takeover")],
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").unwrap();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"open approval over file menu\r");
    dsh.expect(b"waiting with file draft");
    let menu = dsh.checkpoint();
    dsh.write(b"@note");
    dsh.expect_after(menu, b"> @note.txt");

    let takeover = dsh.checkpoint();
    server.release();
    dsh.expect_after(takeover, b"Requested  Patch");
    dsh.expect_after(takeover, b"Proposed update");
    dsh.write(b"\x1b[B\r");
    let approval = dsh.checkpoint();
    dsh.approval_ready();
    dsh.expect_after(approval, b"> Reject");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    let rejected = dsh.checkpoint();
    dsh.write(b"\r");
    dsh.expect_after(rejected, b"Rejected");
    dsh.expect_after(rejected, b"rejected after file takeover");
    dsh.expect_after(rejected, b"Turn complete");
    dsh.expect_after(rejected, b"> @note.txt");
    dsh.write(b"\x15/exit\r");
    dsh.expect(b"> /exit");
    dsh.write(b"\r");
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 2);
}

#[test]
fn enhanced_approval_rejects_printable_same_read_and_pasted_authority() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-enhanced-approval",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("enhanced approval finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"exercise enhanced approval safety\r");
    dsh.approval_ready();

    dsh.write(b"y\r");
    dsh.approval_ready_occurrence(2);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    dsh.write(b"\x1b[A\r");
    dsh.approval_ready_occurrence(3);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    dsh.write(b"\x1b[200~\x1b[A\r\x1b[201~");
    dsh.approval_ready_occurrence(4);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    let newline_selection = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(newline_selection, b"> Allow once");
    dsh.write(b"\n");
    dsh.approval_ready_occurrence(6);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    let selection = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(selection, b"> Allow once");
    dsh.expect_after(selection, b"Arrow keys move | Enter confirms | Esc stops");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"\r");
    dsh.expect(b"Updated  note.txt");
    dsh.expect(b"enhanced approval finished");
    dsh.expect(b"Turn complete");
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(
        transcript
            .windows(b"Updated  note.txt".len())
            .filter(|window| *window == b"Updated  note.txt")
            .count(),
        1
    );
    for sentinel in [b"--- a/note.txt".as_slice(), b"+++ b/note.txt"] {
        assert_eq!(
            transcript
                .windows(sentinel.len())
                .filter(|window| *window == sentinel)
                .count(),
            1,
            "invalid selector input must redraw only the Dock"
        );
    }
    for obsolete in [
        b"Tool requested".as_slice(),
        b"Tool finished",
        b"Allowed once",
    ] {
        assert!(
            !transcript
                .windows(obsolete.len())
                .any(|window| window == obsolete)
        );
    }
    let card = transcript
        .windows(b"Updated  note.txt".len())
        .position(|window| window == b"Updated  note.txt")
        .unwrap();
    let receipt = transcript
        .windows(b"Turn complete".len())
        .position(|window| window == b"Turn complete")
        .unwrap();
    assert!(card < receipt);
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn interactive_help_quit_and_idle_ctrl_d_are_real_terminal_commands() {
    let server = SequenceSseServer::start(Vec::new());
    let workspace = TestWorkspace::new();
    let mut quit = PtyHarness::spawn(&server.base_url, &workspace.0);

    quit.expect(b"dsh | interactive; new session ");
    quit.expect(b"dsh > ");
    let partial = quit.checkpoint();
    quit.write(b"/he");
    std::thread::sleep(Duration::from_millis(100));
    let partial_output = quit.snapshot();
    assert!(
        !partial_output[partial..]
            .windows(b"> /help".len())
            .any(|window| window == b"> /help"),
        "linear mode must not render the enhanced command palette"
    );
    assert!(
        !partial_output.contains(&0x1b),
        "linear mode must remain free of ANSI application-mode output"
    );
    quit.write(b"lp\r");
    quit.expect(b"[commands]");
    quit.expect(b"/focus  return to Focus");
    quit.expect(b"/exit  exit dsh");
    quit.expect_occurrences(b"dsh > ", 2);
    quit.write(b"/quit\r");
    let (status, _) = quit.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert!(server.finish().is_empty());

    let server = SequenceSseServer::start(Vec::new());
    let mut eof = PtyHarness::spawn(&server.base_url, &workspace.0);
    eof.expect(b"dsh > ");
    eof.write(&[0x04]);
    let (status, _) = eof.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert!(server.finish().is_empty());
}

#[test]
fn unsafe_echo_and_output_terminal_modes_are_rejected_before_the_first_prompt() {
    for mode in [
        DisabledTerminalMode::EchoControls,
        DisabledTerminalMode::OutputPostprocess,
        DisabledTerminalMode::OutputNewlineMapping,
    ] {
        let server = SequenceSseServer::start(Vec::new());
        let workspace = TestWorkspace::new();
        let dsh =
            PtyHarness::spawn_with_disabled_terminal_mode(&server.base_url, &workspace.0, mode);
        let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));

        assert_eq!(status.code(), Some(1));
        assert!(
            transcript
                .windows(b"CLI_TERMINAL_UNSUPPORTED".len())
                .any(|window| window == b"CLI_TERMINAL_UNSUPPORTED")
        );
        assert!(!transcript.windows(6).any(|window| window == b"dsh > "));
        assert!(server.finish().is_empty());
    }
}

#[test]
fn real_canonical_terminal_bounds_records_recovers_huge_paste_and_submits_non_lf_veof() {
    let server = SequenceSseServer::start(vec![
        text_sse("exact canonical input accepted"),
        text_sse("veof input accepted"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    let exact = "x".repeat(1_000);
    dsh.write(format!("{exact}\r").as_bytes());
    dsh.expect(b"assistant | exact canonical input accepted");
    dsh.expect_occurrences(b"dsh > ", 2);

    dsh.write(format!("{}\r", "y".repeat(1_001)).as_bytes());
    dsh.expect(b"[input exceeds 1000 bytes]");
    dsh.expect_occurrences(b"dsh > ", 3);

    dsh.write("z".repeat(8 * 1_024).as_bytes());
    // macOS deliberately stops accepting a canonical record when the kernel
    // queue fills, before dsh can observe it. The application must not submit
    // that truncated paste, and the standard Ctrl+C recovery must flush it.
    dsh.expect(&vec![b'z'; 1_023]);
    assert_eq!(
        dsh.snapshot()
            .windows(b"[working; press Ctrl+C to stop]".len())
            .filter(|window| *window == b"[working; press Ctrl+C to stop]")
            .count(),
        1
    );
    dsh.write(&[0x03]);
    dsh.expect_occurrences(b"dsh > ", 4);

    dsh.write(b"veof prompt without newline");
    dsh.write(&[0x04]);
    dsh.expect(b"assistant | veof input accepted");
    dsh.expect_occurrences(b"dsh > ", 5);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(&format!("\"content\":\"{exact}\"")));
    assert!(requests[1].contains("\"content\":\"veof prompt without newline\""));
}

#[test]
fn idle_hup_quit_and_term_use_stable_exit_codes() {
    let workspace = TestWorkspace::new();
    for (signal, expected) in [(Signal::HUP, 129), (Signal::QUIT, 131), (Signal::TERM, 143)] {
        let server = SequenceSseServer::start(Vec::new());
        let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);
        dsh.expect(b"dsh > ");
        dsh.signal(signal);
        let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));

        assert_eq!(status.code(), Some(expected));
        assert!(!transcript.windows(5).any(|bytes| bytes == b"CLI_"));
        assert!(server.finish().is_empty());
    }
}

#[test]
fn active_eof_cancels_the_stalled_turn_and_exits_zero_after_cleanup() {
    let partial =
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial-before-eof\"}}]}\n\n".to_owned();
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"stall until eof\r");
    dsh.expect(b"assistant | partial-before-eof");
    dsh.write(&[0x04]);
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let (request, closed) = server.finish();

    assert!(status.success());
    assert!(closed);
    assert!(request.contains("\"content\":\"stall until eof\""));
}

#[test]
fn active_hup_quit_and_term_cancel_before_their_stable_exit() {
    let workspace = TestWorkspace::new();
    for (signal, expected, marker) in [
        (Signal::HUP, 129, "partial-before-hup"),
        (Signal::QUIT, 131, "partial-before-quit"),
        (Signal::TERM, 143, "partial-before-term"),
    ] {
        let partial = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
            serde_json::to_string(marker).unwrap()
        );
        let server = StalledSseServer::start(partial);
        let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

        dsh.expect(b"dsh > ");
        dsh.write(b"stall for signal\r");
        dsh.expect(format!("assistant | {marker}").as_bytes());
        dsh.signal(signal);
        let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));
        let (request, closed) = server.finish();

        assert_eq!(status.code(), Some(expected));
        assert!(closed);
        assert!(request.contains("\"content\":\"stall for signal\""));
        assert!(!transcript.windows(5).any(|bytes| bytes == b"CLI_"));
    }
}

#[test]
fn a_non_reading_terminal_hits_one_output_deadline_and_exits_without_recursive_diagnostics() {
    let large_delta = "x".repeat(128 * 1_024);
    let partial =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{large_delta}\"}}}}]}}\n\n");
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.pause_reading();
    let started = std::time::Instant::now();
    dsh.write(b"fill the terminal output queue\r");
    let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(8));
    let elapsed = started.elapsed();
    let (request, closed) = server.finish();

    assert_eq!(status.code(), Some(1));
    assert!(
        closed,
        "output failure must cancel and close the provider stream"
    );
    assert!(request.contains("fill the terminal output queue"));
    assert!(
        elapsed >= Duration::from_millis(4_500),
        "elapsed={elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(8), "elapsed={elapsed:?}");
    assert!(
        !transcript.windows(5).any(|bytes| bytes == b"CLI_"),
        "the final error must not write recursively to the blocked terminal"
    );
}

#[test]
fn enhanced_output_deadline_restores_cbreak_and_cancels_the_provider_once() {
    let large_delta = "x".repeat(128 * 1_024);
    let partial =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{large_delta}\"}}}}]}}\n\n");
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect(b"Ready");
    assert!(dsh.terminal_uses_application_mode());
    dsh.pause_reading();
    let started = Instant::now();
    dsh.write(b"block the enhanced screen writer\r");
    let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(8));
    let elapsed = started.elapsed();
    let (request, closed) = server.finish();

    assert_eq!(status.code(), Some(1));
    assert!(closed);
    assert!(request.contains("block the enhanced screen writer"));
    assert!(
        elapsed >= Duration::from_millis(4_500),
        "elapsed={elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(8), "elapsed={elapsed:?}");
    assert!(!transcript.windows(5).any(|bytes| bytes == b"CLI_"));
}

#[test]
fn enhanced_partial_screen_write_preserves_the_terminating_signal_identity() {
    let large_delta = "x".repeat(128 * 1_024);
    let partial =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{large_delta}\"}}}}]}}\n\n");
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect(b"Ready");
    dsh.pause_reading();
    dsh.write(b"interrupt a partial enhanced frame\r");
    std::thread::sleep(Duration::from_millis(150));
    dsh.signal(Signal::TERM);
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let (request, closed) = server.finish();

    assert_eq!(status.code(), Some(143));
    assert!(closed);
    assert!(request.contains("interrupt a partial enhanced frame"));
}

#[test]
fn enhanced_partial_screen_write_restores_termios_before_suspending() {
    let large_delta = "x".repeat(128 * 1_024);
    let partial =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{large_delta}\"}}}}]}}\n\n");
    let server = StalledSseServer::start(partial);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect(b"Ready");
    dsh.pause_reading();
    dsh.write(b"suspend a partial enhanced frame\r");
    std::thread::sleep(Duration::from_millis(150));
    dsh.signal(Signal::TSTP);
    dsh.wait_until_stopped();
    assert_eq!(dsh.terminal_state(), dsh.initial_terminal_state());

    dsh.resume_reading();
    let resumed = dsh.checkpoint();
    dsh.signal(Signal::CONT);
    dsh.expect_after(resumed, b"Ready");
    assert!(dsh.terminal_uses_application_mode());
    let (status, _) = dsh.exit_cleanly();
    let (request, closed) = server.finish();

    assert!(status.success());
    assert!(closed);
    assert!(request.contains("suspend a partial enhanced frame"));
}

#[test]
fn trickle_progress_cannot_restart_the_final_backlog_deadline() {
    let server = SequenceSseServer::start(vec![repeated_text_sse_with_width(3_900, 256)]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_rolling(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.pause_reading();
    let mut trickle = dsh.duplicate_observed_reader();
    let started = Instant::now();
    dsh.write(b"build a maximum bounded final-drain backlog\r");
    let progress = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        let mut scratch = [0_u8; 512];
        trickle
            .read_with_timeout(&mut scratch, Duration::from_secs(1))
            .expect("trickle PTY read should make bounded progress")
    });

    let progressed = progress.join().expect("trickle reader should join");
    let overall_deadline = started + Duration::from_secs(9);
    let (status, transcript) =
        dsh.wait_for_exit(overall_deadline.saturating_duration_since(Instant::now()));
    let elapsed = started.elapsed();
    let requests = server.finish();

    assert_eq!(status.code(), Some(1));
    assert!(progressed > 0, "the PTY must make observable late progress");
    assert!(
        elapsed >= Duration::from_secs(4),
        "the test must reach its deliberately late progress window: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "final drain must use one total deadline instead of restarting per frame: {elapsed:?}"
    );
    assert_eq!(requests.len(), 1);
    assert!(
        !transcript
            .windows(b"CLI_OUTPUT_FAILED".len())
            .any(|window| window == b"CLI_OUTPUT_FAILED"),
        "a broken terminal must not trigger a second blocking diagnostic"
    );
}

#[test]
fn maximum_backlog_signals_discard_without_per_event_output_deadlines() {
    for signal in [Signal::INT, Signal::TSTP, Signal::TERM] {
        let server =
            BacklogThenStalledSseServer::start(text_backlog_then_read_tool_sse(3_800, 256));
        let workspace = TestWorkspace::new();
        std::fs::write(workspace.0.join("note.txt"), "backlog tool input\n")
            .expect("read fixture should be created");
        let mut dsh = PtyHarness::spawn_rolling(&server.base_url, &workspace.0);

        dsh.expect(b"dsh > ");
        dsh.pause_reading();
        dsh.write(b"build a bounded backlog before the signal\r");
        server.wait_until_second_request();
        let signalled = Instant::now();
        dsh.signal(signal);
        dsh.resume_reading();

        match signal {
            Signal::INT => {
                dsh.expect(b"stopped; skipped");
                dsh.expect_occurrences(b"dsh > ", 2);
                assert!(signalled.elapsed() < Duration::from_secs(3));
                let transcript = dsh.snapshot();
                let transcript_text = String::from_utf8_lossy(&transcript);
                let skipped_start = transcript_text
                    .rfind("stopped; skipped ")
                    .expect("stopped summary should include the skipped count")
                    + "stopped; skipped ".len();
                let skipped_end = transcript_text[skipped_start..]
                    .find(" updates")
                    .map(|offset| skipped_start + offset)
                    .expect("stopped summary should close the skipped count");
                let skipped = transcript_text[skipped_start..skipped_end]
                    .parse::<usize>()
                    .expect("stopped summary count should be numeric");
                assert!(
                    skipped >= 3_000,
                    "the test must exercise a maximum-scale committed backlog, got {skipped}"
                );
                assert_eq!(
                    transcript
                        .windows(b"stopped; skipped".len())
                        .filter(|window| *window == b"stopped; skipped")
                        .count(),
                    1
                );
                let (status, _) = dsh.exit_cleanly();
                assert!(status.success());
                let (requests, second_closed) = server.finish();
                assert_eq!(requests.len(), 2);
                assert!(second_closed);
            }
            Signal::TSTP => {
                dsh.wait_until_stopped();
                assert!(signalled.elapsed() < Duration::from_secs(3));
                assert!(
                    !dsh.snapshot()
                        .windows(b"stopped; skipped".len())
                        .any(|window| window == b"stopped; skipped")
                );
                dsh.signal(Signal::CONT);
                dsh.expect_occurrences(b"dsh > ", 2);
                let (status, _) = dsh.exit_cleanly();
                assert!(status.success());
                let (requests, second_closed) = server.finish();
                assert_eq!(requests.len(), 2);
                assert!(second_closed);
            }
            Signal::TERM => {
                let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(3));
                assert_eq!(status.code(), Some(143));
                assert!(signalled.elapsed() < Duration::from_secs(3));
                assert!(
                    !transcript
                        .windows(b"stopped; skipped".len())
                        .any(|window| window == b"stopped; skipped")
                );
                let (requests, second_closed) = server.finish();
                assert_eq!(requests.len(), 2);
                assert!(second_closed);
            }
            _ => unreachable!("the table contains only the three tested signals"),
        }
    }
}

#[test]
fn interactive_dsh_keeps_committed_history_across_two_turns() {
    let server = SequenceSseServer::start(vec![
        text_sse("first committed answer"),
        text_sse("second committed answer"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"first prompt\r");
    dsh.expect(b"assistant | first committed answer");
    dsh.expect_occurrences(b"dsh > ", 2);
    dsh.write(b"second prompt\r");
    dsh.expect(b"assistant | second committed answer");
    dsh.expect_occurrences(b"dsh > ", 3);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"content\":\"first prompt\""));
    assert!(requests[1].contains("\"content\":\"first prompt\""));
    assert!(requests[1].contains("\"content\":\"first committed answer\""));
    assert!(requests[1].contains("\"content\":\"second prompt\""));
}

#[test]
fn interactive_resume_reuses_the_stored_context_and_reaches_a_new_prompt() {
    let server = SequenceSseServer::start(vec![
        text_sse("seeded answer"),
        text_sse("answer after interactive resume"),
    ]);
    let workspace = TestWorkspace::new();
    let caller_workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let seeded = Command::new(env!("CARGO_BIN_EXE_dsh"))
        .args([
            "--prompt",
            "seed this durable session",
            "--model",
            "deepseek-chat",
            "--workspace",
            workspace.0.to_str().unwrap(),
            "--no-color",
        ])
        .env_clear()
        .env("DEEPSEEK_BASE_URL", &server.base_url)
        .env("DEEPSEEK_API_KEY", "test-key-for-loopback-only")
        .env("DSH_SESSION_ROOT", session_root.path())
        .env("HOME", &workspace.0)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .output()
        .expect("seed script should run");
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    assert_eq!(seeded.stdout, b"seeded answer\n");

    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let filename = entries[0].file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();

    let mut dsh = PtyHarness::spawn_resume(
        &server.base_url,
        &caller_workspace.0,
        session_root,
        &session_id,
    );
    dsh.expect(format!("dsh | interactive; resumed session {session_id}").as_bytes());
    dsh.expect(b"dsh > ");
    dsh.write(b"continue interactively\r");
    dsh.expect(b"assistant | answer after interactive resume");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("\"content\":\"seed this durable session\""));
    assert!(requests[1].contains("\"content\":\"seeded answer\""));
    assert!(requests[1].contains("\"content\":\"continue interactively\""));
}

#[test]
fn bare_resume_picker_is_read_only_until_selection_and_reuses_the_real_resume_path() {
    let server = SequenceSseServer::start(vec![
        text_sse("picker seed answer"),
        text_sse("linear picker answer"),
        text_sse("enhanced picker answer"),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let seeded = Command::new(env!("CARGO_BIN_EXE_dsh"))
        .args([
            "--prompt",
            "seed the picker session",
            "--model",
            "deepseek-chat",
            "--workspace",
            workspace.0.to_str().unwrap(),
            "--no-color",
        ])
        .env_clear()
        .env("DEEPSEEK_BASE_URL", &server.base_url)
        .env("DEEPSEEK_API_KEY", "test-key-for-loopback-only")
        .env("DSH_SESSION_ROOT", session_root.path())
        .env("HOME", &workspace.0)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .output()
        .expect("picker seed should run");
    assert!(seeded.status.success());

    let entry = std::fs::read_dir(session_root.path())
        .unwrap()
        .next()
        .expect("one picker session should exist")
        .unwrap();
    let session_id = entry
        .file_name()
        .into_string()
        .unwrap()
        .strip_suffix(".jsonl")
        .unwrap()
        .to_owned();
    let before_cancel = std::fs::read(entry.path()).unwrap();

    let mut cancelled = PtyHarness::spawn_picker_color_with_preloaded_input(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
        b"\r",
    );
    cancelled.expect(b"Resume a session");
    cancelled.signal(Signal::TSTP);
    cancelled.wait_until_stopped();
    assert_eq!(
        cancelled.terminal_state(),
        cancelled.initial_terminal_state()
    );
    let resumed_picker = cancelled.checkpoint();
    cancelled.signal(Signal::CONT);
    cancelled.expect_after(resumed_picker, b"Resume a session");
    let resized_picker = cancelled.checkpoint();
    cancelled.resize(12, 44);
    cancelled.expect_after(resized_picker, b"Resume a session");
    let redraw = cancelled.checkpoint();
    cancelled.write(b"\x1b[B");
    cancelled.expect_after(redraw, b"Resume a session");
    cancelled.write(b"\x1b");
    let (status, _) = cancelled.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert_eq!(std::fs::read(entry.path()).unwrap(), before_cancel);

    let mut interrupted =
        PtyHarness::spawn_picker_color_cargo(&server.base_url, &workspace.0, session_root.clone());
    interrupted.expect(b"Resume a session");
    interrupted.signal(Signal::INT);
    let (status, _) = interrupted.wait_for_exit(Duration::from_secs(5));
    assert_eq!(status.code(), Some(130));
    assert_eq!(std::fs::read(entry.path()).unwrap(), before_cancel);

    let mut blocked = PtyHarness::spawn_picker_color_with_blocked_first_frame(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );
    blocked.wait_for_selector_input_mode();
    blocked.write(b"\r");
    blocked.resume_reading();
    blocked.expect(b"Resume a session");
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !blocked
            .snapshot()
            .windows(b"resumed session".len())
            .any(|window| window == b"resumed session")
    );
    blocked.write(b"\x1b");
    let (status, _) = blocked.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert_eq!(std::fs::read(entry.path()).unwrap(), before_cancel);

    let mut stale_linear = PtyHarness::spawn_picker_linear_with_preloaded_input(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
        b"\r",
    );
    stale_linear.expect(b"Resume a session:");
    stale_linear.write(b"q\r");
    let (status, transcript) = stale_linear.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert!(
        !transcript
            .windows(b"resumed session".len())
            .any(|window| window == b"resumed session")
    );
    assert_eq!(std::fs::read(entry.path()).unwrap(), before_cancel);

    let mut linear =
        PtyHarness::spawn_picker_linear_cargo(&server.base_url, &workspace.0, session_root.clone());
    linear.expect(b"Resume a session:");
    assert!(!linear.snapshot().contains(&b'\x1b'));
    linear.write(b"\r");
    linear.expect(format!("resumed session {session_id}").as_bytes());
    linear.expect(b"dsh > ");
    linear.write(b"continue through linear picker\r");
    linear.expect(b"assistant | linear picker answer");
    linear.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = linear.exit_cleanly();
    assert!(status.success());
    assert!(!transcript.contains(&b'\x1b'));

    let mut enhanced =
        PtyHarness::spawn_picker_color_cargo(&server.base_url, &workspace.0, session_root);
    enhanced.expect(b"Resume a session");
    enhanced.write(b"\r");
    enhanced.expect(format!("resumed session {session_id}").as_bytes());
    enhanced.expect("❯".as_bytes());
    enhanced.write(b"continue through enhanced picker\r");
    enhanced.expect(b"enhanced picker answer");
    enhanced.expect(b"Turn complete");
    let (status, _) = enhanced.exit_cleanly();
    assert!(status.success());

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(last_user_content(&requests[0]), "seed the picker session");
    assert_eq!(
        last_user_content(&requests[1]),
        "continue through linear picker"
    );
    assert_eq!(
        last_user_content(&requests[2]),
        "continue through enhanced picker"
    );
}

#[test]
fn bare_resume_picker_empty_state_exits_without_creating_a_session_or_calling_provider() {
    let server = SequenceSseServer::start(Vec::new());
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    assert!(!session_root.path().exists());

    let mut picker =
        PtyHarness::spawn_picker_color_cargo(&server.base_url, &workspace.0, session_root.clone());
    picker.expect(b"No resumable sessions for this workspace.");
    let (status, transcript) = picker.wait_for_exit(Duration::from_secs(5));

    assert!(status.success());
    assert!(!session_root.path().exists());
    assert!(
        !transcript
            .windows(b"dsh | interactive".len())
            .any(|window| { window == b"dsh | interactive" })
    );
    assert!(server.finish().is_empty());
}

#[test]
fn session_picker_selects_a_nondefault_session_and_eof_is_read_only() {
    let server = SequenceSseServer::start(vec![
        text_sse("older seed answer"),
        text_sse("newer seed answer"),
        text_sse("selected alternate answer"),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut ids = Vec::new();
    let mut id_prompts = Vec::new();
    for prompt in ["older picker context", "newer picker context"] {
        let before_ids = ids.clone();
        let seeded = Command::new(env!("CARGO_BIN_EXE_dsh"))
            .args([
                "--prompt",
                prompt,
                "--model",
                "deepseek-chat",
                "--workspace",
                workspace.0.to_str().unwrap(),
                "--no-color",
            ])
            .env_clear()
            .env("DEEPSEEK_BASE_URL", &server.base_url)
            .env("DEEPSEEK_API_KEY", "test-key-for-loopback-only")
            .env("DSH_SESSION_ROOT", session_root.path())
            .env("HOME", &workspace.0)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .output()
            .expect("picker seed should run");
        assert!(seeded.status.success());
        ids = std::fs::read_dir(session_root.path())
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .into_string()
                    .unwrap()
                    .strip_suffix(".jsonl")
                    .unwrap()
                    .to_owned()
            })
            .collect();
        ids.sort();
        let created = ids
            .iter()
            .find(|id| !before_ids.contains(id))
            .expect("each seed should create one new session")
            .clone();
        id_prompts.push((created, prompt));
    }
    assert_eq!(ids.len(), 2);

    let listed = Command::new(env!("CARGO_BIN_EXE_dsh"))
        .args([
            "--list-sessions",
            "--workspace",
            workspace.0.to_str().unwrap(),
            "--no-color",
        ])
        .env_clear()
        .env("DSH_SESSION_ROOT", session_root.path())
        .env("HOME", &workspace.0)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("session listing should run");
    assert!(listed.status.success());
    let listed_ids = String::from_utf8(listed.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split('\t').next().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(listed_ids.len(), 2);
    let selected_id = listed_ids[1].clone();
    let selected_prompt = id_prompts
        .iter()
        .find_map(|(id, prompt)| (id == &selected_id).then_some(*prompt))
        .expect("listed session should retain its seeded prompt");
    let unselected_prompt = if selected_prompt == "older picker context" {
        "newer picker context"
    } else {
        "older picker context"
    };
    let snapshot = || {
        let mut files = std::fs::read_dir(session_root.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    };
    let before_pick = snapshot();

    let mut picker =
        PtyHarness::spawn_picker_color_cargo(&server.base_url, &workspace.0, session_root.clone());
    picker.expect(b"Resume a session");
    let moved = picker.checkpoint();
    picker.write(b"\x1b[B\r");
    picker.expect_after(moved, b"Resume a session");
    let resized = picker.checkpoint();
    picker.resize(20, 80);
    picker.expect_after(resized, b"Resume a session");
    assert_eq!(snapshot(), before_pick);
    picker.write(b"\r");
    picker.expect(format!("resumed session {selected_id}").as_bytes());
    picker.expect("❯".as_bytes());
    picker.write(b"continue selected alternate\r");
    picker.expect(b"selected alternate answer");
    picker.expect(b"Turn complete");
    let (status, _) = picker.exit_cleanly();
    assert!(status.success());

    let before_eof = snapshot();
    let mut eof =
        PtyHarness::spawn_picker_color_cargo(&server.base_url, &workspace.0, session_root.clone());
    eof.expect(b"Resume a session");
    eof.write(&[0x04]);
    let (status, _) = eof.wait_for_exit(Duration::from_secs(5));
    assert!(status.success());
    assert_eq!(snapshot(), before_eof);

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].contains(selected_prompt));
    assert!(!requests[2].contains(unselected_prompt));
}

#[test]
fn enhanced_theme_and_motion_are_process_local_and_resume_starts_from_defaults() {
    let server = SequenceSseServer::start(vec![text_sse("theme persistence seed")]);
    let workspace = TestWorkspace::new();
    let caller_workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let mut first = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    first.expect("❯".as_bytes());
    first.write(b"/theme paper\r");
    first.expect("Theme changed · paper".as_bytes());
    first.write(b"/motion reduced\r");
    first.expect("Motion changed · reduced".as_bytes());
    first.write(b"persist one ordinary turn\r");
    first.expect(b"theme persistence seed");
    first.expect(b"Turn complete");
    let (status, _) = first.exit_cleanly();
    assert!(status.success());

    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let filename = entries[0].file_name().into_string().unwrap();
    let session_id = filename.strip_suffix(".jsonl").unwrap().to_owned();
    let journal = std::fs::read(entries[0].path()).unwrap();
    assert!(!journal.windows(b"/theme".len()).any(|row| row == b"/theme"));
    assert!(
        !journal
            .windows(b"Theme changed".len())
            .any(|row| row == b"Theme changed")
    );
    assert!(
        !journal
            .windows(b"/motion".len())
            .any(|row| row == b"/motion")
    );
    assert!(
        !journal
            .windows(b"Motion changed".len())
            .any(|row| row == b"Motion changed")
    );

    let mut resumed = PtyHarness::spawn_resume_color_cargo(
        &server.base_url,
        &caller_workspace.0,
        session_root,
        &session_id,
    );
    resumed.expect("❯".as_bytes());
    let shown = resumed.checkpoint();
    resumed.write(b"/theme\r");
    resumed.expect_after(shown, "Theme · adaptive".as_bytes());
    resumed.expect_after(shown, b"\x1b[1;33m");
    let shown = resumed.checkpoint();
    resumed.write(b"/motion\r");
    resumed.expect_after(shown, "Motion · full".as_bytes());
    let (status, _) = resumed.exit_cleanly();

    assert!(status.success());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn long_reasoning_across_three_turns_continues_past_the_old_event_ceiling() {
    let server = SequenceSseServer::start(vec![
        reasoning_sse(1_400, "first long answer"),
        reasoning_sse(1_400, "second long answer"),
        reasoning_sse(1_400, "third long answer"),
        text_sse("answer after the old ceiling"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_rolling(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    for (index, (turn, answer)) in [
        (
            b"first long task".as_slice(),
            b"assistant | first long answer".as_slice(),
        ),
        (
            b"second long task".as_slice(),
            b"assistant | second long answer".as_slice(),
        ),
        (
            b"third long task".as_slice(),
            b"assistant | third long answer".as_slice(),
        ),
        (
            b"continue after long reasoning".as_slice(),
            b"assistant | answer after the old ceiling".as_slice(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        dsh.write(turn);
        dsh.write(b"\r");
        dsh.expect(answer);
        dsh.expect_occurrences(b"dsh > ", index + 2);
    }
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 4);
    assert!(requests[2].contains("first long answer"));
    assert!(requests[2].contains("second long answer"));
    assert!(
        !transcript
            .windows(b"AGENT_EVENT_BUDGET".len())
            .any(|window| window == b"AGENT_EVENT_BUDGET")
    );
}

#[test]
fn durable_session_continues_after_crossing_the_old_real_event_ceiling() {
    let server = SequenceSseServer::start(vec![repeated_text_sse(3_975), repeated_text_sse(120)]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"nearly fill the session event log\r");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);

    dsh.write(b"reach the remaining event ceiling\r");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 3);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert_eq!(
        transcript
            .windows(b"AGENT_EVENT_BUDGET".len())
            .filter(|window| *window == b"AGENT_EVENT_BUDGET")
            .count(),
        0
    );
}

#[test]
fn durable_session_continues_after_crossing_the_old_retained_json_ceiling() {
    let server = SequenceSseServer::start(vec![
        many_invalid_read_calls_sse("large-call-a", 16, 220_000),
        many_invalid_read_calls_sse("large-call-b", 4, 220_000),
        text_sse("answer after the old retained limit"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_rolling(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"cross the retained session byte ceiling with hidden arguments\r");
    dsh.expect(b"assistant | answer after the old retained limit");
    dsh.expect(b"[done]");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("x-deepseek-harness-compact: 1\r\n")
    );
    assert!(requests[2].contains("large-call-a-15"));
    assert_eq!(
        transcript
            .windows(b"AGENT_EVENT_BUDGET".len())
            .filter(|window| *window == b"AGENT_EVENT_BUDGET")
            .count(),
        0
    );
}

#[test]
fn ctrl_c_cancels_a_stalled_stream_and_the_next_turn_still_works() {
    let partial =
        concat!("data: {\"choices\":[{\"delta\":{\"content\":\"partial-before-cancel\"}}]}\n\n",)
            .to_owned();
    let server = CancelThenSseServer::start(partial, text_sse("answer-after-cancel"));
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    let initial_terminal_state = dsh.terminal_state();
    dsh.write(b"stall this turn\r");
    dsh.expect(b"assistant | partial-before-cancel");
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    dsh.expect_occurrences(b"dsh > ", 2);
    dsh.write(b"continue after cancel\r");
    dsh.expect(b"assistant | answer-after-cancel");
    dsh.expect_occurrences(b"dsh > ", 3);
    assert_eq!(dsh.terminal_state(), initial_terminal_state);
    let (status, _) = dsh.exit_cleanly();
    let (requests, first_connection_closed) = server.finish();

    assert!(status.success());
    assert!(first_connection_closed);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"content\":\"stall this turn\""));
    assert!(requests[1].contains("\"content\":\"continue after cancel\""));
    assert!(!requests[1].contains("partial-before-cancel"));
}

#[test]
fn approval_and_suspend_resume_leave_the_real_terminal_state_unchanged() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-terminal-state",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("answer after selector suspension"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    let initial = dsh.terminal_state();
    dsh.write(b"test approval terminal state\r");
    dsh.approval_ready();
    assert_ne!(dsh.terminal_state(), initial);
    dsh.signal(Signal::TSTP);
    dsh.wait_until_stopped();
    assert_eq!(dsh.terminal_state(), initial);
    dsh.signal(Signal::CONT);
    dsh.expect_occurrences(b"dsh > ", 2);
    assert_eq!(dsh.terminal_state(), initial);

    dsh.write(b"continue after selector suspension\r");
    dsh.expect(b"assistant | answer after selector suspension");
    dsh.expect_occurrences(b"dsh > ", 3);

    let (status, _) = dsh.exit_cleanly();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn transient_zero_width_keeps_the_last_safe_approval_geometry() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-zero-width",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("zero width approval completed"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.resize(24, 0);
    dsh.write(b"exercise a zero width terminal\r");
    dsh.approval_ready();
    dsh.expect(b"Reject is the safe default");
    let allow = dsh.checkpoint();
    dsh.write(b"\x1b[A");
    dsh.expect_after(allow, b"> Allow once");
    let reject = dsh.checkpoint();
    dsh.write(b"\x1b[B");
    dsh.expect_after(reject, b"> Reject");
    assert!(
        !dsh.snapshot()
            .windows(b"\x1b[5A".len())
            .any(|window| window == b"\x1b[5A"),
        "a narrow or unknown terminal must append redraws instead of guessing wrapped rows"
    );
    dsh.write(b"\r");
    dsh.expect(b"zero width approval completed");
    dsh.expect_occurrences("❯".as_bytes(), 2);
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn ctrl_z_cleans_an_approved_shell_group_before_bash_fg_resumes_dsh() {
    let server = SequenceSseServer::start(vec![tool_sse(
        "call-job-control",
        "bash",
        serde_json::json!({
            "command": job_control_shell_command(),
            "description": "exercise bounded job cleanup",
            "timeoutMs": 25_000
        }),
    )]);
    let workspace = TestWorkspace::new();
    let mut terminal = JobControlHarness::spawn(&server.base_url, &workspace.0);
    let dsh_group = terminal.start_dsh_job();

    terminal.write(b"run the job-control cleanup fixture\r");
    terminal.approval_ready();
    terminal.write(b"y\r");
    terminal.expect(b"[approval: allowed once]");
    wait_for_file(&workspace.0.join("shell-started"), Duration::from_secs(5));
    let approved_group = terminal.remember_approved_group();
    assert_eq!(terminal.foreground_group(), dsh_group);
    assert_eq!(process_state::is_stopped(dsh_group), Some(false));

    terminal.write(&[0x1a]);
    wait_for_file(&workspace.0.join("cleanup-entered"), Duration::from_secs(5));
    assert_eq!(
        process_state::is_stopped(dsh_group),
        Some(false),
        "dsh must keep supervising its tool while cleanup is still pending"
    );
    assert_eq!(terminal.foreground_group(), dsh_group);
    assert!(rustix::process::test_kill_process_group(approved_group).is_ok());
    std::fs::write(workspace.0.join("cleanup-release"), b"").expect("cleanup gate should release");

    terminal.expect_occurrences(JobControlHarness::SHELL_PROMPT, 3);
    assert_eq!(process_state::is_stopped(dsh_group), Some(true));
    assert_eq!(terminal.foreground_group(), terminal.shell_group());
    assert!(
        terminal.approved_group_is_gone(Duration::from_secs(2)),
        "approved process group must be gone before dsh suspends"
    );

    let background_checkpoint = terminal.checkpoint();
    terminal.write(b"bg %1\r");
    terminal.expect_after(background_checkpoint, JobControlHarness::SHELL_PROMPT);
    let stopped_checkpoint = terminal.checkpoint();
    terminal.write(
        b"limit=$((SECONDS+5)); while ! jobs -s %1 | /usr/bin/grep -q Stopped && [ \"$SECONDS\" -lt \"$limit\" ]; do :; done; jobs -s %1 | /usr/bin/grep -q Stopped && printf 'JC_BG_STOPPED\\n'\r",
    );
    terminal.expect_after(stopped_checkpoint, b"JC_BG_STOPPED");
    assert_eq!(process_state::is_stopped(dsh_group), Some(true));
    assert_eq!(terminal.foreground_group(), terminal.shell_group());
    assert!(
        !terminal.snapshot()[background_checkpoint..]
            .windows(b"dsh > ".len())
            .any(|window| window == b"dsh > "),
        "a background dsh must stop again without writing to the terminal"
    );

    let foreground_checkpoint = terminal.checkpoint();
    terminal.write(b"fg %1\r");
    terminal.expect_after(foreground_checkpoint, b"dsh > ");
    assert_eq!(process_state::is_stopped(dsh_group), Some(false));
    assert_eq!(terminal.foreground_group(), dsh_group);
    let exit_checkpoint = terminal.checkpoint();
    terminal.write(b"/exit\r");
    terminal.expect_after(exit_checkpoint, JobControlHarness::SHELL_PROMPT);
    let status_checkpoint = terminal.checkpoint();
    terminal.write(b"printf 'JC_DSH_STATUS:%s\\n' \"$?\"\r");
    terminal.expect_after(status_checkpoint, b"JC_DSH_STATUS:0");
    let (status, transcript) = terminal.finish_shell(Duration::from_secs(5));
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 1);
    assert!(!transcript.windows(5).any(|bytes| bytes == b"CLI_"));
}

#[test]
fn terminating_signals_override_a_pending_ctrl_z_after_shell_cleanup() {
    for (signal, expected) in [(Signal::HUP, 129), (Signal::QUIT, 131), (Signal::TERM, 143)] {
        let server = SequenceSseServer::start(vec![tool_sse(
            "call-job-control-exit",
            "bash",
            serde_json::json!({
                "command": job_control_shell_command(),
                "description": "exercise terminating signal priority",
                "timeoutMs": 25_000
            }),
        )]);
        let workspace = TestWorkspace::new();
        let mut terminal = JobControlHarness::spawn(&server.base_url, &workspace.0);
        let dsh_group = terminal.start_dsh_job();

        terminal.write(b"run the terminating-signal fixture\r");
        terminal.approval_ready();
        terminal.write(b"y\r");
        wait_for_file(&workspace.0.join("shell-started"), Duration::from_secs(5));
        terminal.remember_approved_group();
        terminal.write(&[0x1a]);
        wait_for_file(&workspace.0.join("cleanup-entered"), Duration::from_secs(5));
        assert_eq!(process_state::is_stopped(dsh_group), Some(false));

        rustix::process::kill_process(dsh_group, signal)
            .expect("owned dsh job should accept the terminating signal");
        // Keep the tool cleanup gate closed briefly so the dispatcher observes
        // the stronger exit intent before the original suspend can settle.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(workspace.0.join("cleanup-release"), b"")
            .expect("cleanup gate should release");

        terminal.expect_occurrences(JobControlHarness::SHELL_PROMPT, 3);
        assert_ne!(process_state::is_stopped(dsh_group), Some(true));
        assert_eq!(terminal.foreground_group(), terminal.shell_group());
        assert!(terminal.approved_group_is_gone(Duration::from_secs(2)));
        terminal.write(b"printf 'JC_DSH_STATUS:%s\\n' \"$?\"\r");
        terminal.expect(format!("JC_DSH_STATUS:{expected}").as_bytes());
        let (status, transcript) = terminal.finish_shell(Duration::from_secs(5));
        let requests = server.finish();

        assert!(status.success());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            transcript
                .windows(b"dsh > ".len())
                .filter(|window| *window == b"dsh > ")
                .count(),
            1,
            "a terminating signal must exit instead of resuming a suspended dsh"
        );
    }
}

#[test]
fn short_approval_answer_allows_one_patch_after_the_preview() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-patch",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("patch finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note\r");
    dsh.expect(b"[approval requested]");
    dsh.expect(b"--- a/note.txt");
    dsh.approval_ready();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "old\n",
        "the file must not change before approval"
    );
    dsh.write(b"y\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"[tool result: success]");
    dsh.expect(b"assistant | patch finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("call-patch"));
}

#[test]
fn arrow_selection_requires_enter_before_it_applies_a_patch() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-arrow-patch",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("arrow selection finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note with the selector\r");
    dsh.approval_ready();
    dsh.write(b"\x1b[A");
    dsh.expect(b"[x] Allow once");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"assistant | arrow selection finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn escape_cancels_the_selector_without_applying_a_patch() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-escape-patch",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("escape cancellation recorded"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"cancel this patch from the selector\r");
    dsh.approval_ready();
    dsh.write(b"\x1b");
    dsh.expect(b"[approval: cancelled]");
    dsh.expect(b"assistant | escape cancellation recorded");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn bracketed_paste_sequence_rearms_before_a_real_selector_choice() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-pasted-patch",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("pasted input never authorized"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"reject bracketed paste authority\r");
    dsh.approval_ready();
    dsh.write(b"\x1b[200~y\r\x1b[201~");
    dsh.approval_ready_occurrence(2);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"y\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"assistant | pasted input never authorized");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn complete_and_partial_input_before_a_fresh_approval_cannot_authorize() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let tool = tool_sse(
        "call-fenced",
        "apply_patch",
        serde_json::json!({ "patch": patch }),
    );
    let split = tool
        .find("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}")
        .expect("tool fixture should contain a final response boundary");
    let mut server = GatedFirstSseServer::start(
        tool[..split].to_owned(),
        tool[split..].to_owned(),
        vec![text_sse("fenced patch finished")],
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note safely\r");
    dsh.expect(b"[working; press Ctrl+C to stop]");
    dsh.write(b"y\rallow stale-partial");
    server.release();

    dsh.approval_ready();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"y\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"assistant | fenced patch finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(requests.len(), 2);
    assert!(
        !transcript
            .windows(b"[approval answer not recognized]".len())
            .any(|bytes| bytes == b"[approval answer not recognized]")
    );
}

#[test]
fn continuous_stale_input_and_selection_without_enter_remain_fail_closed() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let tool = tool_sse(
        "call-stale-flood",
        "apply_patch",
        serde_json::json!({ "patch": patch }),
    );
    let split = tool
        .find("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}")
        .expect("tool fixture should contain a final response boundary");
    let mut server = GatedFirstSseServer::start(
        tool[..split].to_owned(),
        tool[split..].to_owned(),
        vec![text_sse("stale input never authorized the patch")],
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note behind a fresh fence\r");
    dsh.expect(b"[working; press Ctrl+C to stop]");
    let mut stale_writer = dsh.duplicate_writer();
    let flood = std::thread::spawn(move || {
        for _ in 0..250 {
            if stale_writer
                .write_all(b"y\rallow 00000000-0000-4000-8000-000000000000\r")
                .is_err()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    server.release();
    flood.join().expect("stale input writer should join");

    dsh.approval_ready();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"y");
    dsh.expect(b"[x] Allow once");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    dsh.write(b"\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"assistant | stale input never authorized the patch");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert_eq!(requests.len(), 2);
}

#[test]
fn approval_preview_output_failure_is_bounded_and_never_changes_the_file() {
    let addition = "x".repeat(48 * 1_024);
    let patch = format!("--- /dev/null\n+++ b/large-preview.txt\n@@ -0,0 +1 @@\n+{addition}\n");
    let tool = tool_sse(
        "call-preview-output",
        "apply_patch",
        serde_json::json!({ "patch": patch }),
    );
    let split = tool
        .find("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}")
        .expect("tool fixture should contain a final response boundary");
    let mut server = GatedFirstSseServer::start(
        tool[..split].to_owned(),
        tool[split..].to_owned(),
        Vec::new(),
    );
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("large-preview.txt");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"prepare a large patch preview\r");
    dsh.expect(b"[working; press Ctrl+C to stop]");
    dsh.pause_reading();
    let started = std::time::Instant::now();
    server.release();
    let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(7));
    let elapsed = started.elapsed();
    let requests = server.finish();

    assert_eq!(status.code(), Some(1));
    assert_eq!(requests.len(), 1);
    assert!(
        !target.exists(),
        "an unseen approval must never commit a patch"
    );
    assert!(
        elapsed >= Duration::from_millis(4_500),
        "elapsed={elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(7), "elapsed={elapsed:?}");
    assert!(
        !transcript
            .windows(b"[approval required]".len())
            .any(|bytes| bytes == b"[approval required]"),
        "the transcript must not claim the selector became answerable"
    );
}

#[test]
fn default_enter_rejects_a_patch_and_the_session_continues() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-reject",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("rejection recorded"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note\r");
    dsh.expect(b"[approval requested]");
    dsh.approval_ready();
    dsh.write(b"\r");
    dsh.expect(b"[approval: rejected]");
    dsh.expect(b"[tool result: error]");
    dsh.expect(b"assistant | rejection recorded");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("call-reject"));
    assert!(requests[1].to_ascii_lowercase().contains("rejected"));
}

#[test]
fn ctrl_c_at_patch_approval_cancels_without_a_write_and_a_later_turn_works() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-cancel",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("answer after approval cancel"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note\r");
    dsh.approval_ready();
    dsh.write(&[0x03]);
    dsh.expect(b"stopped; skipped");
    dsh.expect_occurrences(b"dsh > ", 2);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");

    dsh.write(b"continue safely\r");
    dsh.expect(b"assistant | answer after approval cancel");
    dsh.expect_occurrences(b"dsh > ", 3);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("\"content\":\"continue safely\""));
}

#[test]
fn ctrl_d_at_patch_approval_cancels_without_a_write_and_exits_zero() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let server = SequenceSseServer::start(vec![tool_sse(
        "call-eof",
        "apply_patch",
        serde_json::json!({ "patch": patch }),
    )]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("note.txt");
    std::fs::write(&target, "old\n").expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"change note\r");
    dsh.approval_ready();
    dsh.write(&[0x04]);
    let (status, _) = dsh.wait_for_exit(Duration::from_secs(5));
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    assert_eq!(requests.len(), 1);
}

#[test]
fn terminating_signals_at_patch_approval_restore_the_terminal_without_a_write() {
    let patch = "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n";
    for (signal, expected_exit) in [(Signal::HUP, 129), (Signal::QUIT, 131), (Signal::TERM, 143)] {
        let server = SequenceSseServer::start(vec![tool_sse(
            "call-terminating-approval",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        )]);
        let workspace = TestWorkspace::new();
        let target = workspace.0.join("note.txt");
        std::fs::write(&target, "old\n").expect("test file should be created");
        let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

        dsh.expect(b"dsh > ");
        dsh.write(b"request a patch, then terminate the terminal turn\r");
        dsh.approval_ready();
        dsh.signal(signal);
        let (status, transcript) = dsh.wait_for_exit(Duration::from_secs(5));
        let requests = server.finish();

        assert_eq!(status.code(), Some(expected_exit));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
        assert_eq!(requests.len(), 1);
        assert!(
            !transcript
                .windows(b"[approval: allowed once]".len())
                .any(|window| window == b"[approval: allowed once]")
        );
    }
}

#[test]
fn foreground_shell_runs_only_after_the_confirmed_terminal_approval() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-shell",
            "bash",
            serde_json::json!({
                "command": "printf shell-ok > shell-result.txt",
                "description": "create a bounded test sentinel",
                "timeoutMs": 25_000
            }),
        ),
        text_sse("shell finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("shell-result.txt");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"run the safe shell command\r");
    dsh.expect(b"[tool requested]");
    dsh.expect(b"tool | bash");
    dsh.approval_ready();
    assert!(!target.exists());
    dsh.write(b"y\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"[tool result: success]");
    dsh.expect(b"assistant | shell finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "shell-ok");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("call-shell"));
}

#[test]
fn background_shell_is_approved_started_and_collected_through_real_terminal_tools() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-background-shell",
            "bash",
            serde_json::json!({
                "command": "sleep 0.05; printf terminal-background-ok",
                "description": "produce delayed terminal background output",
                "timeoutMs": 2000,
                "run_in_background": true
            }),
        ),
        tool_sse(
            "call-background-output",
            "job_output",
            serde_json::json!({
                "job_id": "bash-1",
                "wait": true,
                "timeout_ms": 2000
            }),
        ),
        tool_sse(
            "call-background-output-repeat",
            "job_output",
            serde_json::json!({ "job_id": "bash-1" }),
        ),
        text_sse("background shell collected"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"run and collect the background command\r");
    dsh.expect(b"tool | bash");
    dsh.approval_ready();
    dsh.write(b"y\r");
    dsh.expect(b"[approval: allowed once]");
    dsh.expect(b"[tool result: success]");
    dsh.expect(b"tool | job_output");
    dsh.expect(b"assistant | background shell collected");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 4);
    assert_eq!(
        tool_message_content(&requests[1], "call-background-shell"),
        "started background job bash-1"
    );
    let output = tool_message_content(&requests[2], "call-background-output");
    assert!(output.contains("terminal-background-ok"));
    assert!(output.contains("[status: completed, exit code: 0]"));
    assert_eq!(
        tool_message_content(&requests[3], "call-background-output-repeat"),
        "(no new output)\n[status: completed, exit code: 0]"
    );
}

#[test]
fn idle_background_completion_opens_one_notice_turn_in_the_real_terminal() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-background-wake",
            "bash",
            serde_json::json!({
                "command": "sleep 1; printf idle-wake-output",
                "description": "finish after the first turn becomes idle",
                "timeoutMs": 3000,
                "run_in_background": true
            }),
        ),
        text_sse("background work is running"),
        text_sse("background completion noticed"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"start the delayed background command\r");
    dsh.approval_ready();
    dsh.write(b"y\r");
    dsh.expect(b"background work is running");
    dsh.expect(b"background completion noticed");
    dsh.expect_occurrences(b"dsh > ", 3);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 3);
    assert!(requests[2].contains("tool-jobs"), "{}", requests[2]);
    assert!(requests[2].contains("background job bash-1"));
    assert!(requests[2].contains("Read its output with job_output"));
}

#[test]
fn foreground_shell_spills_full_output_and_returns_its_private_locator_to_the_model() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-shell-spill",
            "bash",
            serde_json::json!({
                "command": "i=1; while [ $i -le 8000 ]; do printf 'line-%04d\\n' $i; i=$((i + 1)); done",
                "description": "produce a bounded overflowing stdout stream",
                "timeoutMs": 25_000
            }),
        ),
        text_sse("large shell output retained"),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"run the bounded large-output command\r");
    dsh.approval_ready();
    dsh.write(b"y\r");
    dsh.expect(b"[tool result: success]");
    dsh.expect(b"assistant | large shell output retained");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    let content = tool_message_content(&requests[1], "call-shell-spill");
    assert!(content.contains("line-8000"));
    assert!(!content.contains("line-0001"));
    let marker = "[output truncated; full output: ";
    let locator = content
        .split_once(marker)
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(path, _)| path)
        .expect("tool result should return one spill locator");
    let spill_path = std::path::PathBuf::from(locator);
    let full = std::fs::read_to_string(&spill_path).unwrap();
    assert!(full.starts_with("line-0001\n"));
    assert!(full.ends_with("line-8000\n"));
    let spill_directory = spill_path.parent().unwrap().to_owned();
    std::fs::remove_file(spill_path).unwrap();
    std::fs::remove_dir(spill_directory).unwrap();
}

#[test]
fn exact_shell_process_choice_runs_a_normalized_repeat_without_a_second_prompt() {
    let command = "printf x >> shell-result.txt";
    let server = SequenceSseServer::start(vec![
        two_tool_sse(
            (
                "call-shell-exact-1",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "first display reason",
                    "timeoutMs": 25_000
                }),
            ),
            (
                "call-shell-exact-2",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "different display reason",
                    "workdir": "."
                }),
            ),
        ),
        text_sse("both exact shell calls finished"),
    ]);
    let workspace = TestWorkspace::new();
    let session_root = TestSessionRoot::new();
    let target = workspace.0.join("shell-result.txt");
    let mut dsh = PtyHarness::spawn_color_with_session_root_cargo(
        &server.base_url,
        &workspace.0,
        session_root.clone(),
    );

    dsh.expect("❯".as_bytes());
    dsh.write(b"run the exact command twice\r");
    dsh.approval_ready();
    dsh.expect(b"Allow exact Shell");
    assert!(!target.exists());
    dsh.write(b"\x1b[B");
    dsh.expect(b"> Allow exact Shell");
    assert!(!target.exists());
    dsh.write(b"\r");
    // Reaching the final answer proves the second call did not stop on a new
    // selector. Both calls still execute and produce their own result.
    dsh.expect(b"both exact shell calls finished");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "xx");
    assert_eq!(requests.len(), 2);
    let entries = std::fs::read_dir(session_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let journal = std::fs::read(entries[0].path()).unwrap();
    for event in [
        b"\"type\":\"approval/asked\"".as_slice(),
        b"\"type\":\"approval/decided\"".as_slice(),
    ] {
        assert_eq!(
            journal
                .windows(event.len())
                .filter(|row| *row == event)
                .count(),
            1
        );
    }
    assert_eq!(
        journal
            .windows(b"\"type\":\"tool/result\"".len())
            .filter(|row| *row == b"\"type\":\"tool/result\"")
            .count(),
        2
    );
}

#[test]
fn exact_background_shell_choice_reuses_only_the_same_detached_shape() {
    // Keep both jobs live until the test exits. An instant command races the
    // separate completion-notice contract and makes this approval-scope test
    // depend on scheduler timing instead of the exact detached identity.
    let command = "printf x >> background-exact.txt; sleep 2";
    let server = SequenceSseServer::start(vec![
        two_tool_sse(
            (
                "call-background-exact-1",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "first background display reason",
                    "timeoutMs": 5000,
                    "run_in_background": true
                }),
            ),
            (
                "call-background-exact-2",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "second background display reason",
                    "timeoutMs": 5000,
                    "run_in_background": true
                }),
            ),
        ),
        text_sse("both background starts accepted"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("background-exact.txt");
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"start the exact background command twice\r");
    dsh.approval_ready();
    dsh.expect(b"Allow exact Shell");
    dsh.write(b"\x1b[B");
    dsh.expect(b"> Allow exact Shell");
    dsh.write(b"\r");
    dsh.expect(b"both background starts accepted");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "xx");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn linear_exact_shell_process_choice_requires_a_fresh_confirmation() {
    let command = "printf y >> linear-shell-result.txt";
    let server = SequenceSseServer::start(vec![
        two_tool_sse(
            (
                "call-linear-exact-1",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "first linear reason"
                }),
            ),
            (
                "call-linear-exact-2",
                "bash",
                serde_json::json!({
                    "command": command,
                    "description": "second linear reason"
                }),
            ),
        ),
        text_sse("linear exact calls finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("linear-shell-result.txt");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"run the exact linear command twice\r");
    dsh.approval_ready();
    dsh.expect(b"Allow exact Shell for this process");
    dsh.write(b"\x1b[B");
    dsh.expect(b"[x] Allow exact Shell for this process");
    assert!(!target.exists());
    dsh.write(b"\n");
    dsh.expect(b"assistant | linear exact calls finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "yy");
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn auto_edit_mode_keeps_foreground_shell_behind_terminal_approval() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-auto-edit-shell",
            "bash",
            serde_json::json!({
                "command": "printf shell-ok > shell-result.txt",
                "description": "prove auto-edit does not authorize shell",
                "timeoutMs": 25_000
            }),
        ),
        text_sse("auto-edit shell finished"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join("shell-result.txt");
    let mut dsh =
        PtyHarness::spawn_color_with_approval_mode(&server.base_url, &workspace.0, "auto-edit");

    dsh.expect("❯".as_bytes());
    dsh.write(b"run shell while auto-edit is enabled\r");
    dsh.approval_ready();
    assert!(!target.exists());
    dsh.write(b"\x1b[A");
    dsh.expect(b"> Allow once");
    assert!(!target.exists());
    dsh.write(b"\r");
    dsh.expect(b"auto-edit shell finished");
    dsh.expect(b"Turn complete");
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "shell-ok");
    assert_eq!(requests.len(), 2);
}

#[test]
fn read_only_tool_status_and_result_reach_the_real_terminal() {
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-read",
            "read",
            serde_json::json!({ "file_path": "note.txt" }),
        ),
        text_sse("read finished"),
    ]);
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("note.txt"), "read-only sentinel\n")
        .expect("test file should be created");
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"read note\r");
    dsh.expect(b"[tool requested]");
    dsh.expect(b"tool | read");
    dsh.expect(b"[tool result: success]");
    dsh.expect(b"assistant | read finished");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, _) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("read-only sentinel"));
}

#[test]
fn model_terminal_controls_are_rendered_as_visible_plain_text() {
    let malicious = "safe\u{1b}]52;c;clipboard\u{7}\u{202e}end";
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-visible-tool",
            "unknown\u{202e}",
            serde_json::json!({}),
        ),
        text_sse(malicious),
    ]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"show unsafe text\r");
    dsh.expect(b"tool | unknown\\u{202e}");
    dsh.expect(b"assistant | safe\\u{1b}]52;c;clipboard\\u{7}\\u{202e}end");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert_eq!(requests.len(), 2);
    assert!(!transcript.contains(&0x1b));
    assert!(
        !transcript
            .windows(3)
            .any(|bytes| bytes == [0xe2, 0x80, 0xae])
    );
}

#[test]
fn styled_terminal_never_treats_model_controls_as_product_ansi() {
    let malicious = "safe\u{1b}]52;c;clipboard\u{7}\u{202e}end";
    let server = SequenceSseServer::start(vec![text_sse(malicious)]);
    let workspace = TestWorkspace::new();
    let mut dsh = PtyHarness::spawn_color(&server.base_url, &workspace.0);

    dsh.expect("❯".as_bytes());
    dsh.write(b"render hostile model text safely\r");
    dsh.expect(b"safe\\u{1b}]52;c;clipboard\\u{7}\\u{202e}end");
    dsh.expect_occurrences("❯".as_bytes(), 2);
    let (status, transcript) = dsh.exit_cleanly();

    assert!(status.success());
    assert!(
        transcript.contains(&0x1b),
        "product styling should be present"
    );
    assert!(
        !transcript
            .windows(b"\x1b]52".len())
            .any(|window| window == b"\x1b]52"),
        "model OSC bytes must never reach the terminal"
    );
    assert!(
        !transcript
            .windows("\u{202e}".len())
            .any(|window| window == "\u{202e}".as_bytes()),
        "model bidi controls must remain visible escapes"
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn approval_preview_and_path_bidi_are_visible_text_not_terminal_controls() {
    let unsafe_name = "note\u{202e}.txt";
    let unsafe_line = "safe\u{202e}end";
    let patch = format!("--- /dev/null\n+++ b/{unsafe_name}\n@@ -0,0 +1 @@\n+{unsafe_line}\n");
    let server = SequenceSseServer::start(vec![
        tool_sse(
            "call-visible-preview",
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        ),
        text_sse("unsafe preview rejected"),
    ]);
    let workspace = TestWorkspace::new();
    let target = workspace.0.join(unsafe_name);
    let mut dsh = PtyHarness::spawn(&server.base_url, &workspace.0);

    dsh.expect(b"dsh > ");
    dsh.write(b"show a control-safe patch preview\r");
    dsh.approval_ready();
    let transcript = dsh.snapshot();
    assert!(
        transcript
            .windows(b"note\\u{202e}.txt".len())
            .any(|window| window == b"note\\u{202e}.txt")
    );
    assert_eq!(
        transcript
            .windows(b"preview | +safe\\u{202e}end".len())
            .filter(|window| *window == b"preview | +safe\\u{202e}end")
            .count(),
        1
    );
    assert!(
        transcript
            .windows(b"safe\\u{202e}end".len())
            .any(|window| window == b"safe\\u{202e}end")
    );
    assert!(!transcript.contains(&0x1b));
    assert!(
        !transcript
            .windows("\u{202e}".len())
            .any(|window| window == "\u{202e}".as_bytes())
    );

    dsh.write(b"n\r");
    dsh.expect(b"assistant | unsafe preview rejected");
    dsh.expect_occurrences(b"dsh > ", 2);
    let (status, transcript) = dsh.exit_cleanly();
    let requests = server.finish();

    assert!(status.success());
    assert!(!target.exists());
    assert_eq!(requests.len(), 2);
    assert!(!transcript.contains(&0x1b));
}

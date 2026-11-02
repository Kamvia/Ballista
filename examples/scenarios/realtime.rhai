// File: ballista/examples/scenarios/realtime.rhai

fn run(step, res) {
    switch step {
        0 => ws("/ws", json_encode(#{ type: "ping", at: now_ms() }), "echo"),
        1 => {
            if !res.body.starts_with("echo:") {
                throw "unexpected websocket reply";
            }
            ()
        }
    }
}

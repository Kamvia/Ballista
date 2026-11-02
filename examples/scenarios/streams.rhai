// File: ballista/examples/scenarios/streams.rhai

fn tasks() {
    #{ prices: 1, chat: 2 }
}

// Server-sent events: reads 6 events across reconnects, resuming with Last-Event-ID.
fn prices(step, res) {
    switch step {
        0 => #{ path: "/sse", name: "price stream", sse: 6, reconnects: 3 },
        1 => {
            if res.events.len() < 6 { throw `only ${res.events.len()} events`; }
            let last = res.events[res.events.len() - 1];
            log(`last seq ${last.json.seq} after ${res.reconnects} reconnects`);
        }
    }
}

// Socket.IO: the session stays open for the user across iterations.
fn chat(step, res) {
    switch step {
        0 => #{ socketio: "/socket.io/", emit: "echo", data: #{ text: "hi", n: rand_int(1, 9) }, ack: true, name: "echo ack" },
        1 => {
            if res.json.text != "hi" { throw "bad ack"; }
            #{ socketio: "/socket.io/", emit: "ping", wait: "pong", name: "ping/pong" }
        }
        2 => sio_ack("echo", [1, 2, 3]),
    }
}

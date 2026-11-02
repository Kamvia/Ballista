// File: ballista/examples/scenarios/protocols.rhai

fn tasks() {
    #{
        http_extras: #{ weight: 2, tags: ["http"] },
        grpc_call: #{ weight: 2, tags: ["grpc"] },
        mqtt_round_trip: #{ weight: 1, tags: ["mqtt"] },
        sql_select: #{ weight: 2, tags: ["sql"] },
        raw_sockets: #{ weight: 1, tags: ["net"] }
    }
}

fn http_extras(step, res) {
    switch step {
        0 => #{
            method: "POST",
            path: "/upload",
            name: "upload",
            multipart: #{ note: "hello", attachment: #{ file: "files/sample.txt", content_type: "text/plain" } },
            check: |r| r.json.parts.len() == 2
        },
        1 => #{ path: "/events", name: "event stream", lines: 12, check: |r| r.lines.len() == 12 },
        2 => #{ path: "/flaky", name: "flaky", retries: 3 },
    }
}

fn grpc_call(step, res) {
    switch step {
        0 => #{ grpc: "shop.Inventory/GetItem", message: #{ id: rand_int(1, 50) }, name: "GetItem" },
        1 => {
            if res.json.name == () { throw "missing item name"; }
            #{ grpc: "shop.Inventory/ListItems", message: #{ id: 5 }, name: "ListItems", max_messages: 100 }
        }
        2 => {
            if res.count != 5 { throw `server stream returned ${res.count}`; }
            #{ grpc: "shop.Inventory/Restock", message: [#{ id: 1, stock: 2 }, #{ id: 2, stock: 3 }], name: "Restock" }
        }
        3 => {
            if res.json.totalStock != "5" { throw `client stream total ${res.json.totalStock}`; }
            #{ grpc: "shop.Inventory/Watch", message: [#{ id: 1 }, #{ id: 2 }, #{ id: 3 }], name: "Watch" }
        }
        4 => {
            if res.count != 3 || res.messages[2].stock != "6" { throw "bidi stream mismatch"; }
        }
    }
}

fn mqtt_round_trip(step, res) {
    switch step {
        0 => {
            let topic = `devices/${this.vu_id}/echo`;
            #{ mqtt: topic, payload: json_encode(#{ t: now_ms() }), qos: 1, reply: topic, name: "echo" }
        }
    }
}

fn sql_select(step, res) {
    switch step {
        0 => #{ sql: "SELECT id, name, price, active FROM items WHERE id = $1", params: [rand_int(1, 100)], name: "item by id" },
        1 => {
            if res.count != 1 { throw "expected one row"; }
            ()
        }
    }
}

fn raw_sockets(step, res) {
    switch step {
        0 => #{ tcp: "127.0.0.1:7000", send: "PING\n", read_until: "\n", name: "tcp echo" },
        1 => #{ udp: "127.0.0.1:7001", send: "ping", name: "udp echo" },
    }
}

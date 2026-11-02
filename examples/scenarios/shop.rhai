// File: ballista/examples/scenarios/shop.rhai

fn tasks() {
    #{
        browse: #{ weight: 6, tags: ["catalog"] },
        view_item: #{ weight: 3, tags: ["catalog"] },
        checkout: #{ weight: 1, tags: ["checkout"] }
    }
}

// Runs before every request map (including batch items) with `this` bound to the user.
fn before_request(req) {
    if req.headers == () {
        req.headers = #{};
    }
    req.headers["x-load-source"] = "ballista";
    if this.token != () {
        req.headers.authorization = `Bearer ${this.token}`;
    }
    req
}

fn on_start(step, res) {
    switch step {
        0 => {
            this.token = ();
            let user = feed("users");
            this.username = user.username;
            post_json("/login", #{ username: user.username, password: user.password }, "login")
        }
        1 => {
            this.token = res.json.token;
            shared_set("last_login", this.username);
            ()
        }
    }
}

fn browse(step, res) {
    switch step {
        0 => #{ batch: [
                #{ path: "/items", name: "list items", query: #{ q: rand_pick(["", "item-1", "item-2"]) } },
                #{ path: "/items/1", name: "item detail" },
                #{ path: "/slow", name: "slow page" }
            ], concurrency: 3, name: "catalog_page" },
        1 => sleep(rand_int(100, 400)),
    }
}

fn view_item(step, res) {
    switch step {
        0 => get("/items", "list items"),
        1 => {
            let items = res.json.items;
            if items.len() == 0 { return; }
            let id = rand_pick(items).id;
            #{
                method: "GET",
                path: `/items/${id}`,
                name: "item detail",
                context: #{ item_id: id },
                check: |r| if r.json.stock < 0 { "negative stock" } else { true }
            }
        }
    }
}

fn checkout(step, res) {
    switch step {
        0 => txn_begin("checkout"),
        1 => get("/items", "list items"),
        2 => #{
            method: "POST",
            path: "/cart",
            name: "add to cart",
            json: #{ item: rand_int(1, 20), qty: rand_int(1, 3), request_id: uuid(), last_login: shared_get("last_login") },
            expect: 201
        },
        3 => txn_end("checkout"),
    }
}

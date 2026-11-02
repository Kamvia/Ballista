// File: ballista/sdks/php/examples/assets/upload.rhai

fn run(step, res) {
    switch step {
        0 => {
            let row = feed("notes");
            #{
                method: "POST",
                path: "/upload",
                name: "upload",
                multipart: #{ note: row.note, attachment: #{ file: "files/report.txt", content_type: "text/plain" } },
                check: |r| r.json.parts.len() == 2
            }
        }
    }
}

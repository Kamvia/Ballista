// File: ballista/examples/shapes/wave.rhai

// Target users at `t` seconds; return () to end the run.
fn shape(t) {
    if t > 60.0 {
        return ();
    }
    let base = 20.0;
    let swing = 15.0;
    base + swing * (t / 10.0).sin()
}

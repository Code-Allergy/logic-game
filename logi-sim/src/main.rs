use logi_sim::graphviz::DotBackend;
use logi_sim::prelude::*;

fn main() -> std::io::Result<()> {
    let mut c = Circuit::new();

    let a = c.and();
    let b = c.or();
    let n = c.not();

    c.connect((a, 0), (b, 0));
    c.connect((n, 0), (b, 1));

    let dot = DotBackend::render(&c);
    std::fs::write("circuit.dot", dot)?;
    Ok(())
}

use std::{env, fs};

use tpt_kinetix_test_utils::trace::{first_divergence, from_json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let left_path = args
        .next()
        .ok_or("usage: trace_diff <left.json> <right.json>")?;
    let right_path = args
        .next()
        .ok_or("usage: trace_diff <left.json> <right.json>")?;
    let left = from_json(&fs::read_to_string(left_path)?)?;
    let right = from_json(&fs::read_to_string(right_path)?)?;
    match first_divergence(&left, &right) {
        Some((key, left, right)) => {
            println!("first divergence: {key}\nleft:  {left:?}\nright: {right:?}");
            std::process::exit(1);
        }
        None => println!("traces are identical"),
    }
    Ok(())
}

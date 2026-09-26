//! Prints the H3 resolution-5 cell of a point: `cargo run -p dedan-core --example cell -- 41.0082 28.9784`.
//! The devnet's nodes.json carries these cells so attesters can post them without an H3 library.
use dedan_core::geo;
use dedan_core::types::LatLon;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (lat, lon): (f64, f64) = (args[1].parse().unwrap(), args[2].parse().unwrap());
    let cell = geo::cell(LatLon::new(lat, lon)).unwrap();
    println!("{} {:#x}", cell.0, cell.0);
}

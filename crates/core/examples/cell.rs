//! Prints the H3 cell of a point and, with a third argument, its k-ring disk (the cells a node joins):
//!   cargo run -p dedan-core --example cell -- 41.0082 28.9784      -> the cell
//!   cargo run -p dedan-core --example cell -- 41.0082 28.9784 1    -> the cell and its 6 neighbours
use dedan_core::geo;
use dedan_core::types::LatLon;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (lat, lon): (f64, f64) = (args[1].parse().unwrap(), args[2].parse().unwrap());
    let cell = geo::cell(LatLon::new(lat, lon)).unwrap();
    match args.get(3).and_then(|k| k.parse::<u32>().ok()) {
        None => println!("{} {:x}", cell.0, cell.0),
        Some(k) => {
            for c in geo::disk(cell, k) {
                println!("{} {:x}", c.0, c.0);
            }
        }
    }
}

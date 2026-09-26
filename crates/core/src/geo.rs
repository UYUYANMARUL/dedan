//! H3 cells (ARCHITECTURE.md §6): resolution 2, about 300 km across, the regions nodes join and humans attest
//! from, and the little geometry the map needs.

use crate::types::{Cell, LatLon};
use h3o::{CellIndex, LatLng, Resolution};

pub const RESOLUTION: Resolution = Resolution::Two;

const EARTH_RADIUS_KM: f64 = 6371.0088;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GeoError {
    #[error("latitude or longitude out of range")]
    Coordinates,
    #[error("not an H3 cell index")]
    Cell,
}

pub fn cell(p: LatLon) -> Result<Cell, GeoError> {
    let ll = LatLng::new(p.lat, p.lon).map_err(|_| GeoError::Coordinates)?;
    Ok(Cell(u64::from(ll.to_cell(RESOLUTION))))
}

fn index(c: Cell) -> Result<CellIndex, GeoError> {
    CellIndex::try_from(c.0).map_err(|_| GeoError::Cell)
}

/// The cell and every cell within `k` rings of it (`k = 0` is the cell alone). Empty for a bad index.
pub fn disk(c: Cell, k: u32) -> Vec<Cell> {
    index(c)
        .map(|i| {
            i.grid_disk::<Vec<_>>(k)
                .into_iter()
                .map(|x| Cell(u64::from(x)))
                .collect()
        })
        .unwrap_or_default()
}

/// The cell's outline as lat/lon vertices, for drawing it; empty for a bad index.
pub fn boundary(c: Cell) -> Vec<LatLon> {
    index(c)
        .map(|i| {
            i.boundary()
                .iter()
                .map(|ll| LatLon::new(ll.lat(), ll.lng()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn centroid(c: Cell) -> Option<LatLon> {
    let ll = LatLng::from(index(c).ok()?);
    Some(LatLon::new(ll.lat(), ll.lng()))
}

/// Great-circle distance, haversine.
pub fn distance_km(a: LatLon, b: LatLon) -> f64 {
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = la2 - la1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod fixtures {
    use super::*;

    pub const ISTANBUL: LatLon = LatLon {
        lat: 41.0082,
        lon: 28.9784,
    };
    pub const FRANKFURT: LatLon = LatLon {
        lat: 50.1109,
        lon: 8.6821,
    };
    pub const TOKYO: LatLon = LatLon {
        lat: 35.6762,
        lon: 139.6503,
    };
    pub const SEOUL: LatLon = LatLon {
        lat: 37.5665,
        lon: 126.978,
    };
    pub const NEWYORK: LatLon = LatLon {
        lat: 40.7128,
        lon: -74.006,
    };

    pub fn c(p: LatLon) -> Cell {
        cell(p).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn known_points_map_to_distinct_resolution_2_cells() {
        let cities = [
            ISTANBUL,
            FRANKFURT,
            TOKYO,
            SEOUL,
            NEWYORK,
            LatLon::new(48.8566, 2.3522), // Paris
            LatLon::new(52.52, 13.405),   // Berlin
        ];
        let cells: std::collections::BTreeSet<Cell> = cities.iter().map(|p| c(*p)).collect();
        assert_eq!(cells.len(), cities.len(), "every devnet city is its own region");
        assert_eq!(CellIndex::try_from(c(ISTANBUL).0).unwrap().resolution(), RESOLUTION);
        // a point 20 km away lands in the same or an adjacent cell, never further
        let near = c(LatLon::new(41.0082 + 0.18, 28.9784));
        assert!(disk(c(ISTANBUL), 1).contains(&near));
        // Berlin is Frankfurt's neighbour
        assert!(disk(c(FRANKFURT), 1).contains(&c(LatLon::new(52.52, 13.405))));
    }

    #[test]
    fn disk_and_centroid_behave() {
        let ist = c(ISTANBUL);
        let ring1 = disk(ist, 1);
        assert_eq!(ring1.len(), 7);
        assert!(ring1.contains(&ist));
        assert_eq!(disk(ist, 0), vec![ist]);
        assert!(disk(Cell(0), 1).is_empty());
        let centre = centroid(ist).unwrap();
        assert!(distance_km(centre, ISTANBUL) < 200.0, "a resolution-2 cell is about 300 km across");
        let outline = boundary(ist);
        assert!(outline.len() >= 5 && outline.len() <= 10, "a hexagon (or a pentagon), {}", outline.len());
        assert!(outline.iter().all(|p| distance_km(*p, centre) < 250.0));
        assert!(boundary(Cell(0)).is_empty());
    }

    #[test]
    fn distances_are_realistic() {
        let d = distance_km(FRANKFURT, ISTANBUL);
        assert!((1840.0..1900.0).contains(&d), "{d}");
        assert!(distance_km(TOKYO, TOKYO) < 1e-9);
    }
}

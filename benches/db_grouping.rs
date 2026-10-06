// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Benchmarks for `LibraryDb::all_albums()` and `LibraryDb::all_artists()`
//! grouping performance at various dataset sizes.

use aulos::library::{LibraryDb, Track};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Create a test track with the given index, distributing tracks across
/// albums and artists to simulate a realistic library.
fn make_track(i: usize) -> Track {
    let album_idx = i / 10; // 10 tracks per album
    let artist_idx = album_idx / 5; // 5 albums per artist
    Track {
        id: 0,
        path: PathBuf::from(format!("/music/track_{i}.flac")),
        title: format!("Track {i}"),
        artist: format!("Artist {artist_idx}"),
        album_artist: format!("Artist {artist_idx}"),
        album: format!("Album {album_idx}"),
        genre: "Rock".into(),
        track_number: (i % 10 + 1) as u32,
        disc_number: 1,
        year: 2020 + (artist_idx % 5) as u32,
        duration: Duration::from_secs(180 + (i % 120) as u64),
        bitrate: 320,
        sample_rate: 44100,
        provider_id: Arc::from("local"),
        source_uri: format!("/music/track_{i}.flac"),
        is_favorite: false,
        rating: None,
        rg_track_gain: None,
        rg_album_gain: None,
    }
}

/// Populate an in-memory DB with `n` tracks and return it.
fn setup_db(n: usize) -> LibraryDb {
    let db = LibraryDb::open_memory().expect("open in-memory DB");
    for i in 0..n {
        let track = make_track(i);
        db.upsert_track(&track, i as i64).expect("upsert track");
    }
    db
}

fn bench_all_albums(c: &mut Criterion) {
    let mut group = c.benchmark_group("all_albums");
    for size in [1_000, 5_000, 10_000] {
        let db = setup_db(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &db, |b, db| {
            b.iter(|| db.all_albums(None).unwrap());
        });
    }
    group.finish();
}

fn bench_all_artists(c: &mut Criterion) {
    let mut group = c.benchmark_group("all_artists");
    for size in [1_000, 5_000, 10_000] {
        let db = setup_db(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &db, |b, db| {
            b.iter(|| db.all_artists(None).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, bench_all_albums, bench_all_artists);
criterion_main!(benches);

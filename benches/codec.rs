//! Encode/decode throughput benchmarks for [`codec`].

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use marlin_binary_transfer::codec::{decode, encode, fletcher16, Packet};

fn bench_fletcher16(c: &mut Criterion) {
    let mut group = c.benchmark_group("fletcher16");
    for &size in &[16usize, 256, 4096] {
        let buf = vec![0xA5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| fletcher16(black_box(&buf)));
        });
    }
    group.finish();
}

fn bench_encode_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet");
    for &size in &[16usize, 256, 4096] {
        let payload = vec![0xA5u8; size];
        let pkt = Packet::new(0, 1, 3, &payload).unwrap();
        let mut encoded = Vec::with_capacity(8 + size + 2);
        encode(&pkt, &mut encoded).unwrap();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("encode/{size}B"), |b| {
            b.iter(|| {
                encoded.clear();
                encode(black_box(&pkt), &mut encoded).unwrap();
            });
        });
        group.bench_function(format!("decode/{size}B"), |b| {
            b.iter(|| {
                let _ = decode(black_box(&encoded)).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_fletcher16, bench_encode_decode);
criterion_main!(benches);

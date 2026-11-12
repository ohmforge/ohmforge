//! Criterion bench for the engineering-notation value parser ([`Value::parse`]),
//! a hot path on every circuit load and shell edit. Benches a representative mix
//! covering plain numbers, SI prefixes, the `meg` word, embedded-decimal (`4k7`,
//! `2n2`) and sign handling.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use ohmforge_core::{Unit, Value};

fn bench_value_parse(c: &mut Criterion) {
    let inputs = [
        ("4k7", Unit::Ohm),
        ("100n", Unit::Farad),
        ("1meg", Unit::Ohm),
        ("3.3V", Unit::Volt),
        ("-2n2", Unit::Farad),
    ];
    c.bench_function("value_parse/mix", |b| {
        b.iter(|| {
            for &(s, unit) in black_box(&inputs) {
                black_box(Value::parse(black_box(s), unit).unwrap());
            }
        })
    });
}

criterion_group!(benches, bench_value_parse);
criterion_main!(benches);

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

type Encoder = fn(&str) -> String;

static ENCODERS: [(&str, Encoder); 4] = [
    ("punyenc", punyenc::encode),
    ("punycode", |input| punycode::encode(input).unwrap()),
    ("idna", |input| idna::punycode::encode_str(input).unwrap()),
    ("punycode-rs", punycode_rs::encode),
];

fn bench_inputs(c: &mut Criterion, group_name: &str, inputs: &[(String, String)]) {
    let mut group = c.benchmark_group(group_name);
    for (case, input) in inputs {
        // Validate the comparison outside the timed region.
        let expected = idna::punycode::encode_str(input).unwrap();
        group.throughput(Throughput::Bytes(input.len() as u64));
        for &(name, encode) in &ENCODERS {
            assert_eq!(encode(input), expected, "{group_name}/{name}/{case}");
            group.bench_with_input(BenchmarkId::new(name, case), input, |b, input| {
                b.iter(|| black_box(encode(black_box(input.as_str()))));
            });
        }
    }
    group.finish();
}

fn short_inputs(c: &mut Criterion) {
    let inputs = [
        ("ascii", "Hello-World_123"),
        ("latin", "bücher"),
        ("identifier", "gödel"),
        ("japanese", "なぜみんな日本語を話してくれないのか"),
        ("arabic", "ليهمابتكلموشعربي؟"),
        ("emoji", "hello-🦀🌍🚀"),
        ("combining", "cafe\u{301}"),
    ]
    .into_iter()
    .map(|(name, input)| (name.to_owned(), input.to_owned()))
    .collect::<Vec<_>>();

    bench_inputs(c, "short", &inputs);
}

fn scaling_inputs(c: &mut Criterion) {
    let mut inputs = Vec::new();
    for len in [16, 64, 256, 1024, 4096] {
        inputs.push((format!("repeated/{len}"), "ü".repeat(len)));
        inputs.push((
            format!("mixed/{len}"),
            "aü中ж".chars().cycle().take(len).collect(),
        ));
        inputs.push((
            format!("distinct-sorted/{len}"),
            (0..len)
                .map(|i| char::from_u32(0x4e00 + i as u32).unwrap())
                .collect(),
        ));
        inputs.push((
            format!("distinct-permuted/{len}"),
            // An odd stride permutes each power-of-two range without repeats.
            (0..len)
                .map(|i| char::from_u32(0x4e00 + ((i * 4051) % len) as u32).unwrap())
                .collect(),
        ));
    }

    bench_inputs(c, "scaling", &inputs);
}

criterion_group!(benches, short_inputs, scaling_inputs);
criterion_main!(benches);

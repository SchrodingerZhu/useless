# punyenc

A Punycode encoder using sorted code points and a Fenwick tree to encode in
`O(n log n)` time with `O(n)` auxiliary space, where `n` is the number of Unicode
code points in the input.

```rust
assert_eq!(punyenc::encode("bücher"), "bcher-kva");
assert_eq!(punyenc::encode("gödel"), "gdel-5qa");
assert_eq!(punyenc::encode("abc"), "abc-");
assert_eq!(punyenc::encode(""), "");
```

`encode` returns raw RFC 3492 Punycode. ASCII characters are preserved, with a
`-` delimiter appended when the input contains any ASCII characters. This crate
only encodes; it does not decode, normalize Unicode, split domain labels, or add
the `xn--` prefix used by IDNA.

## Benchmarks

Run the encoder comparison with:

```sh
cargo bench -p punyenc --bench encode
```

This compares `punyenc` with `punycode` 0.4.1, `idna` 1.1.0, and `punycode-rs`
0.1.0. All four receive the same UTF-8 strings and return newly allocated raw
Punycode strings; the `idna` comparison uses `idna::punycode::encode_str`.
Output equality is checked before timing, and input construction is excluded.

The `short` group covers ASCII, multilingual text, emoji, and combining marks.
The `scaling` group covers repeated characters, mixed scripts, and distinct
code points in sorted and permuted order, at 16–4,096 code points. Throughput
is reported in input UTF-8 bytes per second. Filter a group by appending
`-- short` or `-- scaling`; append `-- --test` to smoke-test all cases without
collecting timings. Criterion writes reports under `target/criterion/`.

## Performance

This crate is for encoding poentially long input on a diversified alphabet, where
the quadratic complexity triggered by rescanning become a large overhead. For small
and simple input that are typical for email addresses, `idna` or other crates may
become more suitable. One may also consider dispatch the implementation based on
input lengths.

Measured on an AMD Ryzen AI Max+ 395. Throughput is in MB/s (1 MB = 1,000,000 bytes).

| Input | Code points | `punyenc` (MB/s) | `punycode` 0.4.1 (MB/s) | `idna` 1.1.0 (MB/s) | `punycode-rs` 0.1.0 (MB/s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 15 | 224.27 | 162.29 | **272.09** | 140.26 |
| Latin (`bücher`) | 6 | 89.27 | 105.40 | **269.06** | 59.69 |
| Identifier (`gödel`) | 5 | 85.78 | 103.60 | **238.27** | 53.60 |
| Japanese | 18 | 122.06 | **134.65** | 108.46 | 71.35 |
| Arabic | 17 | 91.90 | **121.59** | 101.48 | 56.18 |
| ASCII + emoji | 9 | 122.76 | 132.22 | **295.97** | 97.32 |
| Combining mark | 5 | 84.86 | 102.39 | **241.63** | 53.39 |
| Repeated (`ü`) | 16 | 128.82 | 223.99 | **339.26** | 79.58 |
| Repeated (`ü`) | 64 | 152.74 | 340.14 | **373.85** | 119.34 |
| Repeated (`ü`) | 256 | 193.92 | 384.32 | **393.55** | 141.28 |
| Repeated (`ü`) | 1,024 | 209.12 | **438.83** | 396.21 | 153.29 |
| Repeated (`ü`) | 4,096 | 203.65 | **470.40** | 398.17 | 159.14 |
| Mixed (`aü中ж`) | 16 | 118.07 | 183.82 | **260.85** | 71.84 |
| Mixed (`aü中ж`) | 64 | 128.24 | 297.15 | **305.13** | 119.66 |
| Mixed (`aü中ж`) | 256 | 156.06 | **340.16** | 324.50 | 149.50 |
| Mixed (`aü中ж`) | 1,024 | 160.99 | **404.54** | 333.75 | 171.37 |
| Mixed (`aü中ж`) | 4,096 | 143.08 | **425.48** | 337.53 | 180.75 |
| Distinct, sorted | 16 | **167.48** | 164.19 | 117.90 | 80.14 |
| Distinct, sorted | 64 | **206.02** | 66.25 | 25.28 | 50.40 |
| Distinct, sorted | 256 | **222.06** | 14.73 | 7.72 | 16.95 |
| Distinct, sorted | 1,024 | **225.91** | 4.19 | 2.00 | 5.03 |
| Distinct, sorted | 4,096 | **189.89** | 1.08 | 0.54 | 1.53 |
| Distinct, permuted | 16 | 154.44 | **158.38** | 112.55 | 76.73 |
| Distinct, permuted | 64 | **143.44** | 67.24 | 27.11 | 52.40 |
| Distinct, permuted | 256 | **155.25** | 14.51 | 6.94 | 16.54 |
| Distinct, permuted | 1,024 | **144.41** | 4.18 | 1.90 | 4.97 |
| Distinct, permuted | 4,096 | **98.81** | 1.09 | 0.46 | 1.45 |

## License

Licensed under MIT OR Apache-2.0.

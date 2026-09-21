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

Licensed under MIT OR Apache-2.0.

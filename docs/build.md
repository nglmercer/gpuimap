# Build record

The initial Windows proof-of-concept is pinned to:

| Component | Version / target |
| --- | --- |
| Rust | `1.97.1` |
| GPUI | `0.2.2` |
| Host target | `x86_64-pc-windows-msvc` |
| Supported OS | Windows 10/11 x86_64 |

GPUI is pre-1.0. Keep its version pinned and review API changes explicitly before upgrading it. Platform-specific code belongs in the application/UI and location crates; the map domain must remain portable.

## Required checks

The repository includes Windows CI for formatting, workspace tests, clippy with warnings denied, and `cargo audit`. Repository administrators should protect `main` and require these checks before merging:

- `Windows / fmt`
- `Windows / test`
- `Windows / clippy`
- `Security / cargo-audit`

Branch-protection rules are hosted-repository settings rather than source files, so they must be enabled in the GitHub repository settings.

# UniDeps

UniDeps is a deterministic C/C++ dependency orchestrator with native CMake integration, content-addressed caching, patch support, and cross-compilation

## Features

- **CMake native integration**: dependencies are fetched, built and imported at CMake configure time with a single `unideps_setup()` call.
- **Content-addressed cache**: every build is keyed by its source (git URL + ref/commit, local source contents, patches) and its configuration (target, compiler, toolchain, runtime, flags, options, transitive dependencies).
- **Compiler isolation**: separate builds for `clang`, `msvc` and `gcc`, MSVC runtimes, and C++ standard libraries prevent ABI clashes.
- **Cross-compilation**: target dependencies use the project's toolchain; host tools (code generators, assemblers) are built for the host and put on `PATH`.
- **Patch management**: unified diffs (`patches = [...]`) are applied to an isolated copy of the source.

## Installation

Download a binary from the [releases](../../releases) page, or build from source (Rust 1.88+):

```bash
cargo install --path crates/unideps-cli
```

UniDeps drives `git`, `cmake` (3.20+) and `ninja`; all three must be on `PATH`.

## Quick Start

### 1. Define dependencies (`unideps.toml`)

```toml
[package]
name = "my_project"
version = "0.1.0"

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt.git", tag = "10.2.1", cmake_options = { FMT_TEST = "OFF" } }
zlib = { git = "https://github.com/madler/zlib.git", tag = "v1.3.1", auto_import = true }
```

### 2. CMake integration

Copy [`cmake/unideps.cmake`](cmake/unideps.cmake) into your project and call `unideps_setup()` after `project()`:

```cmake
cmake_minimum_required(VERSION 3.20)
project(my_project CXX)

include(cmake/unideps.cmake)
unideps_setup()

add_executable(my_project src/main.cpp)
target_link_libraries(my_project PRIVATE fmt::fmt zlib::zlib)
```

Packages that install a CMake config (like `fmt`) are imported with `find_package(... CONFIG)` automatically. For packages without one (like `zlib`), `auto_import = true` synthesises a `<name>::<name>` target from the installed headers and libraries.

See [`examples/zlib_test`](examples/zlib_test) for a complete project.

## CLI Usage

`unideps_setup()` runs the CLI for you; you only need it directly for scripting or debugging.

```bash
unideps build --manifest unideps.toml --target x86_64-pc-windows-msvc --build-type Debug --compiler clang
```

- `unideps build`: fetch, build and install everything, then write the CMake targets file.
- `unideps fetch`: download all git sources without building.
- `unideps clean`: remove scratch build directories (sources, installed packages and cache are kept).

Run `unideps <command> --help` for all options, or see [DOCUMENTATION.md](DOCUMENTATION.md).

## Storage

By default everything lives in `~/.unideps` and is shared between projects. Override it with `--base-dir`, the `UNIDEPS_DIR` environment variable, `unideps_setup(BASE_DIR ...)`, or `storage.base_dir` in `.local.toml`.

| Directory | Contents |
|---|---|
| `sources/` | Git checkouts and patched source trees |
| `scratch/` | Build directories (`unideps clean` wipes this) |
| `installed/` | Installed package prefixes |
| `cache/` | Compressed `.tar.zst` package archives |
| `logs/` | configure/build/install logs per package |
| `locks/` | Lock files that serialise concurrent runs |

## License

MIT

# Third-Party Notices

## dglab-kit

The built-in waveform catalog in `shared/official-waveforms.json` is generated
from `dglab-kit` 1.0.5 (`COYOTE_WAVEFORM` and `COYOTE_WAVEFORMS`), published by
[dungeonlab-open/dglab-kit](https://github.com/dungeonlab-open/dglab-kit).
The catalog is compiled into the Rust core and also used by the browser preview.
`scripts/generate-waveforms.mjs` reproduces the generated catalog without changing
waveform data. The package's license is GNU GPL version 3; the complete license
text is distributed at `docs/licenses/dglab-kit-GPL-3.0.txt`.

## @dg-kit/waveforms

The browser demonstration parser uses `@dg-kit/waveforms` 1.16.3 from
[0xNullAI/0xNuller](https://github.com/0xNullAI/0xNuller/tree/main/packages/kit/waveforms),
licensed under the MIT License. The Rust `.pulse` parser adapts the package's
parser and validates it against the existing project fixtures. The complete
copyright and permission notice is distributed at `docs/licenses/dg-kit-MIT.txt`.

## Model Context Protocol Rust SDK

The dedicated MCP program's local Streamable HTTP and stdio transports use
`rmcp`, the official
[Model Context Protocol Rust SDK](https://github.com/modelcontextprotocol/rust-sdk),
published as `rmcp` 3.4.0 under the Apache License, Version 2.0. The upstream
license also retains MIT terms for older contributions awaiting relicensing;
the complete upstream notice is distributed at `docs/licenses/rmcp-LICENSE.txt`.

## clap

The CLI, core and MCP argument parsers use [clap](https://github.com/clap-rs/clap), dual-licensed
under the MIT License or the Apache License, Version 2.0.
The license texts are distributed at `docs/licenses/clap-MIT.txt` and
`docs/licenses/clap-APACHE-2.0.txt`.

## DG-LAB-VRCOSC

The built-in breathing test waveform is adapted from
[ccvrc/DG-LAB-VRCOSC](https://github.com/ccvrc/DG-LAB-VRCOSC).

BSD 3-Clause License

Copyright (c) 2024, vrc.cc

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

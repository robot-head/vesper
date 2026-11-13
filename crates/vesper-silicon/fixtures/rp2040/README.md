# RP2040 CMSIS-SVD (vendor file)

`RP2040.svd` is the unmodified vendor SVD for the Raspberry Pi RP2040,
copied from the Raspberry Pi Pico SDK:

- Source: https://github.com/raspberrypi/pico-sdk/blob/079c6f39023649b154152db30f1d781e884879bc/src/rp2040/hardware_regs/RP2040.svd
  (`master` was at `079c6f39023649b154152db30f1d781e884879bc` when the file was fetched on 2026-11-13)
- SHA-256: `1c72330127ae097c8c9a3661b509fcb9a94826d76a5c0d7e259eb605ddd7b0a6`
- Size: 2 592 282 bytes
- Licence: BSD-3-Clause, Copyright (c) 2021 Raspberry Pi Ltd. (stated in the
  file's header comment and `<licenseText>`; reproduced below).

It was chosen because it uses the SVD access annotations the Silicon model
checks: 302 `modifiedWriteValues=oneToClear` (write-1-to-clear) fields and
47 `readAction=modify` fields (FIFO pops, latching timer reads, hardware
spinlocks, UART/SPI data registers). It has no `readAction=clear` field; the
hand-written `../vsim32.svd` covers that case.

`pac.rs` is generated from it by `vesper_silicon::pac::generate_rust`
(the `rp2040` test checks that a fresh generation is identical).

```
Copyright (c) 2021 Raspberry Pi Ltd.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
   this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software
   without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
```

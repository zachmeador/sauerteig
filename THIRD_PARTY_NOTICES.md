# Third-party notices

Sauerteig is MIT licensed. Dependencies retain their own licenses.

## earcutr 0.4.3

Used to triangulate exact subdivision cut faces. Source: https://github.com/frewsxcv/earcutr

```text
ISC License

Copyright (c) 2016, Mapbox
Copyright (c) 2018, Tree Cricket

Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted, provided that the above copyright notice
and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS
OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.
```

## Sauerbraten movement model

The Sauerbraten movement style is a Rust port of the velocity model in the
Sauerbraten engine's `physics.cpp` (`modifyvelocity`, `modifygravity`, and the
velocity part of `moveplayer`; source at
https://svn.code.sf.net/p/sauerbraten/code/src/engine/physics.cpp), with its
constants from `ents.h`. It lives in `crates/core/src/movement.rs` and
`crates/core/src/physics.rs`; the test reference in `crates/core/tests/oracle/`
repeats the same functions in the original's units. The engine's source code is
under the zlib license below. The game media it ships with is not covered by that
license, and Sauerteig uses none of it.

```text
Sauerbraten game engine source code, any release.

Copyright (C) 2001-2020 Wouter van Oortmerssen, Lee Salzman, Mike Dysart, Robert Pointon, and Quinton Reeves

This software is provided 'as-is', without any express or implied
warranty.  In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not
   claim that you wrote the original software. If you use this software
   in a product, an acknowledgment in the product documentation would be
   appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

## Materials and props

The photographic surfaces and five prop models are from Poly Haven, under
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/), as described by
[Poly Haven's asset license](https://polyhaven.com/license). Original authors,
asset URLs, download checksums, modifications and cooked measurements are recorded
in [the material manifest](assets/materials/manifest.json) and
[the prop manifest](assets/props/manifest.json). Copies of
these manifests ship in `asset-provenance/` in the native app's Resources. The
authored hazard/amber utility surfaces are also released under CC0. Sauerteig's
MIT code license does not replace those records.

The optional offline conversion tools use Pillow, NumPy and meshoptimizer;
these tools are not shipped as game dependencies. No source Blender scripts,
legacy map configuration scripts, or third-party game engines run in the client.

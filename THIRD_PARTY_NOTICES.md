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

## Materials and props

The photographic surfaces and five prop models are from Poly Haven, under
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/), as described by
[Poly Haven's asset license](https://polyhaven.com/license). Original authors,
asset URLs, download checksums, modifications and cooked measurements are recorded
in [the material manifest](assets/materials/manifest.json) and
[the prop manifest](assets/props/manifest.json). Copies of
these manifests ship in `asset-provenance/` in the browser build and in the
native app's Resources. The authored hazard/amber utility surfaces are also
released under CC0. Sauerteig's MIT code license does not replace those records.

The optional offline conversion tools use Pillow, NumPy and meshoptimizer;
these tools are not shipped as game dependencies. No source Blender scripts,
legacy map configuration scripts, or third-party game engines run in the client.

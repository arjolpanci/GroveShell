# Icon attribution

The icons under `svg/` are from [Lucide](https://lucide.dev)
(<https://github.com/lucide-icons/lucide>), fetched via the public
[Iconify](https://iconify.design) API and used unmodified apart from
recoloring `currentColor` to white.

Lucide is a fork of Feather Icons and is distributed under the ISC
license:

```
ISC License

Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part of Feather (MIT). All other copyright (c) for Lucide are held by Lucide Contributors 2022.

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted, provided that the
above copyright notice and this permission notice appear in all
copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

`png/` holds the same icons pre-rendered at 128×128 (white-on-transparent)
and is what `apps/ui/src/imp/icons.rs` actually embeds via `include_bytes!`
— `svg/` is kept only as the source of truth if they ever need
re-rendering at a different size.

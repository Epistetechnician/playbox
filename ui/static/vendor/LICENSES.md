# Vendored libraries (served by the hub itself; nothing is loaded from a CDN)

| File | Library | Version | Licence |
|---|---|---|---|
| `katex/` | KaTeX (math) | 0.16.11 | MIT |
| `purify.min.js` | DOMPurify (HTML sanitiser) | 3.2.4 | Apache-2.0 or MPL-2.0 |
| `shiki.bundle.js` | Shiki 1.29.2 (syntax highlighting; JavaScript regex engine, no WebAssembly) with the github-dark and github-light themes and the javascript, typescript, python, rust, shellscript, json, diff, lean, markdown, yaml and toml grammars, bundled with esbuild | 1.29.2 | MIT |

Rebuild: `npm i katex@0.16.11 dompurify@3.2.4 shiki@1.29.2 esbuild`, bundle `shiki/core`, `shiki/engine/javascript`, the grammars and the two themes into one IIFE that sets `window.PBShiki`.
Why these: a chat-thread renderer from a React stack would need a bundler; these three are single files and fit a hub with no build step.

---

## Copyright & License Notices

### KaTeX (0.16.11) — MIT License
Copyright (c) 2013-2020 Khan Academy and other contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

### Shiki (1.29.2) — MIT License
Copyright (c) 2021 Pine Wu; Copyright (c) 2023-present Anthony Fu

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

### DOMPurify (3.2.4) — Apache-2.0 OR MPL-2.0
Copyright 2015 Mario Heiderich

Licensed under the Apache License, Version 2.0 (see the repository root `LICENSE`) or the Mozilla Public License Version 2.0 (https://www.mozilla.org/MPL/2.0/), at your option.

# Vendored libraries (served by the hub itself; nothing is loaded from a CDN)

| File | Library | Version | Licence |
|---|---|---|---|
| `katex/` | KaTeX (math) | 0.16.11 | MIT |
| `purify.min.js` | DOMPurify (HTML sanitiser) | 3.2.4 | Apache-2.0 or MPL-2.0 |
| `shiki.bundle.js` | Shiki 1.29.2 (syntax highlighting; JavaScript regex engine, no WebAssembly) with the github-dark and github-light themes and the javascript, typescript, python, rust, shellscript, json, diff, lean, markdown, yaml and toml grammars, bundled with esbuild | 1.29.2 | MIT |

Rebuild: `npm i katex@0.16.11 dompurify@3.2.4 shiki@1.29.2 esbuild`, bundle `shiki/core`, `shiki/engine/javascript`, the grammars and the two themes into one IIFE that sets `window.PBShiki`.
Why these: a chat-thread renderer from a React stack would need a bundler; these three are single files and fit a hub with no build step.

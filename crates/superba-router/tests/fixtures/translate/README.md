# Translation stream fixtures

The four native success/error streams cover reasoning, vendor state, images,
function calls and usage. Golden snapshots cover all twelve conversion directions.

`gemini.sse` uses complete `functionCall.args`. `gemini-partial.sse` carries the
same call in successive `partialArgs` / `willContinue` messages; the input chunks
split a string argument, retaining its call id and thought signature. Both decode
to exactly the same IR. All three Gemini-source goldens and fake-upstream routes
use the partial fixture. Byte-by-byte delivery is also tested independently.

The partial-argument fields follow Google's [FunctionCall](https://googleapis.github.io/js-genai/release_docs/interfaces/types.FunctionCall.html)
and [PartialArg](https://googleapis.github.io/js-genai/release_docs/interfaces/types.PartialArg.html)
types. These are Vertex/gateway extensions; the Gemini Developer generateContent
API normally sends complete calls. The router accepts these incoming fields but
does not enable them on native Developer API requests. JSON paths are restricted
to singular named children and bounded array indexes; unsupported queries fail.

# Google Gemini API — image generation (+ text) from a Rust backend via plain HTTPS/reqwest

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

Do NOT build on Imagen: the whole Imagen family is deprecated and shuts down August 17, 2026 — 32 days from today (2026-07-16). Image generation is now "Nano Banana" = Gemini native image gen, with four model IDs; `gemini-3.1-flash-image` (Nano Banana 2) is the documented go-to, and `gemini-3.1-flash-lite-image` is the cheap option for avatars/icons at $0.0336/1K image. There are two REST surfaces: the legacy-named but v1-STABLE `POST /v1/models/{model}:generateContent`, and the newer v1beta-only `POST /v1beta/interactions`. For a Rust/reqwest backend I recommend generateContent on v1 — it is on the stable channel and returns errors as a plain object, whereas /v1beta/interactions wraps errors in a JSON ARRAY (`[{"error":{...}}]`), which I verified live and which will break a naive serde struct. Images always come back base64 inline (never a URL). The single most important finding for Atlas's key-validation story: an invalid API key returns HTTP 400 / INVALID_ARGUMENT, NOT 401/403 — indistinguishable from a malformed request by status code alone, so you must branch on `error.details[].reason == "API_KEY_INVALID"`. Also critical: image generation has NO free tier, so a valid free-tier key passes models.list but fails image gen on quota — "key is valid" and "key can generate images" are different questions.

## Implementation notes

RECOMMENDATION: use `POST /v1/models/{model}:generateContent` (v1 stable), NOT `/v1beta/interactions`. Reasons: (1) v1 stable vs v1beta-only; (2) interactions array-wraps errors, which is a deserialization trap; (3) interactions doesn't support custom safety settings; (4) the same generateContent code path serves both image gen and text card-descriptions — one client, one error type. Use `gemini-3.1-flash-image` as the default and `gemini-3.1-flash-lite-image` for high-volume avatars/icons. Do NOT implement Imagen — it's dead in 32 days.

Endpoint construction:
```rust
const BASE: &str = "https://generativelanguage.googleapis.com/v1";
let url = format!("{BASE}/models/{model}:generateContent");
let resp = client.post(&url)
    .header("x-goog-api-key", &api_key)   // NOT ?key= — keeps secret out of URLs/logs
    .json(&req).send().await?;
```

Request (image):
```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenReq { contents: Vec<Content>, generation_config: Option<GenerationConfig> }
#[derive(Serialize)]
struct Content { parts: Vec<Part> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")] response_modalities: Option<Vec<String>>, // ["TEXT","IMAGE"]
    #[serde(skip_serializing_if = "Option::is_none")] image_config: Option<ImageConfig>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageConfig { aspect_ratio: String, image_size: String } // "1:1", "1K"
```
Wire form for a 1:1 1K avatar:
{"contents":[{"parts":[{"text":"..."}]}],"generationConfig":{"responseModalities":["TEXT","IMAGE"],"imageConfig":{"aspectRatio":"1:1","imageSize":"1K"}}}

Response decode — images are base64 inline, interleaved with text, so ITERATE parts and take the first inlineData; never index parts[0]:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenResp { candidates: Option<Vec<Candidate>>, prompt_feedback: Option<PromptFeedback> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate { content: Option<Content>, finish_reason: Option<String> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartOut { text: Option<String>, inline_data: Option<Blob> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Blob { mime_type: String, data: String } // data = base64, decode with base64::engine::general_purpose::STANDARD
```

ERROR CLASSIFICATION — this is the load-bearing part. Do NOT branch on HTTP status alone; invalid key is 400/INVALID_ARGUMENT, identical in status to a malformed body. Branch on details[].reason:
```rust
#[derive(Deserialize)]
struct ApiErrEnvelope { error: ApiErr }
#[derive(Deserialize)]
struct ApiErr {
    code: u16,
    message: String,
    status: String,
    #[serde(default)] details: Vec<ErrDetail>, // ABSENT on the 403 no-key case — must default
}
#[derive(Deserialize)]
struct ErrDetail { #[serde(rename = "@type")] typ: Option<String>, reason: Option<String> }

pub enum GeminiError { InvalidKey, QuotaExceeded, BillingRequired, BadRequest(String), Transient(String) }

fn classify(status: u16, e: &ApiErr) -> GeminiError {
    let reason = e.details.iter().filter_map(|d| d.reason.as_deref()).next();
    match (status, e.status.as_str(), reason) {
        (_, _, Some("API_KEY_INVALID"))       => GeminiError::InvalidKey,   // 400! not 401/403
        (403, "PERMISSION_DENIED", _)          => GeminiError::InvalidKey,   // wrong/missing key
        (429, _, _)                            => GeminiError::QuotaExceeded,
        (400, "FAILED_PRECONDITION", _)        => GeminiError::BillingRequired,
        (500..=504, _, _)                      => GeminiError::Transient(e.message.clone()),
        (400, _, _)                            => GeminiError::BadRequest(e.message.clone()),
        _                                      => GeminiError::Transient(e.message.clone()),
    }
}
```
Retry ONLY Transient and QuotaExceeded (429/5xx), exponential backoff ~1s initial, 60s cap, 4 attempts — mirrors the official SDK. NEVER retry InvalidKey/BadRequest.

KEY VALIDATION (cheap, zero tokens) — `GET /v1beta/models?pageSize=1` with the x-goog-api-key header. 200 => key good; 400+API_KEY_INVALID => warn "invalid/expired key". Note this endpoint is v1beta (v1 also works). IMPORTANT CAVEAT: this proves the key is valid, NOT that it can generate images — image gen has no free tier, so a free-tier key returns 200 here and then 429 on generation. Atlas should word the warning accordingly, and treat a 429 on image gen as "this key has no paid image quota — enable billing" rather than "slow down", since retrying a free-tier key will never succeed. Consider validating lazily/cached (e.g. on key entry + once per day), not per request.

SAFETY HANDLING — a 200 OK can contain no image. After decode: if prompt_feedback.block_reason is Some(_), the prompt was rejected (SAFETY/BLOCKLIST/PROHIBITED_CONTENT/IMAGE_SAFETY) — surface "prompt rejected, rephrase". If candidates exist but no part has inline_data, check finish_reason for IMAGE_SAFETY / IMAGE_PROHIBITED_CONTENT / PROHIBITED_CONTENT and surface a distinct "generation blocked" message. Treat "no image in a 200" as a first-class outcome in your enum, not an unwrap.

COUNT: there is no sampleCount/candidateCount for Nano Banana. For "give me 4 avatar options", fire 4 concurrent requests (join_all) and let the user pick; budget cost as 4x. At $0.0336/image on flash-lite, a 4-option avatar picker costs ~$0.13.

TEXT (card descriptions): identical client, `POST /v1/models/gemini-3.1-flash-lite:generateContent` with {"contents":[{"parts":[{"text":"..."}]}]}. Has a free tier. For typed output set generationConfig.responseMimeType="application/json" + responseSchema so Atlas gets structured descriptions instead of parsing prose.

Store the returned mimeType alongside the decoded bytes (it can be image/png or image/jpeg) rather than hardcoding .png.

## Facts

- **[verified]** Imagen is deprecated and shuts down August 17, 2026 (32 days from today, 2026-07-16). Docs: 'Imagen models are deprecated and will shut down on August 17, 2026. We recommend migrating to Nano Banana for image generation.' Model IDs were imagen-4.0-generate-001 / imagen-4.0-fast-generate-001 / imagen-4.0-ultra-generate-001, via POST /v1beta/models/imagen-4.0-generate-001:predict with {instances:[{prompt}],parameters:{sampleCount}}. Atlas should NOT implement this path.
  - Evidence: https://ai.google.dev/gemini-api/docs/imagen.md.txt lines 4, 17
- **[verified]** Current image model IDs (Nano Banana family): `gemini-3.1-flash-image` (Nano Banana 2, the versatile generalist workhorse), `gemini-3.1-flash-lite-image` (Nano Banana 2 Lite, fastest+cheapest, 1K resolution ONLY, not optimized for multi-reference or multi-turn editing), `gemini-3-pro-image` (Nano Banana Pro, premium, Google Search grounding, thinking), `gemini-2.5-flash-image` (legacy Nano Banana; docs 'strongly recommend' migrating off it).
  - Evidence: https://ai.google.dev/gemini-api/docs/image-generation.md.txt lines 26-35; models.md.txt lists all four as Stable
- **[verified]** Model recommendation for Atlas cover art/avatars/icons: docs say Gemini 3.1 Flash Image 'should be your go-to image generation model, as the best all around performance and intelligence to cost and latency balance.' For avatars/icons (small, 1K, high volume) gemini-3.1-flash-lite-image at $0.0336 is the cost pick. Use gemini-3-pro-image only for cover art with heavy legible text. Docs' own icon example prompt: 'An icon representing a cute dog. The background is white. Make the icons in a colorful and tactile 3D style. No text.' (generated by Nano Banana Pro).
  - Evidence: image-generation.md.txt lines 2504-2528 (Model selection), line 18
- **[verified]** RECOMMENDED endpoint for Rust: POST https://generativelanguage.googleapis.com/v1/models/gemini-3.1-flash-image:generateContent — note this is v1 STABLE, not v1beta. Verbatim body from docs: {"contents":[{"parts":[{"text":"Create a picture of a nano banana dish in a fancy restaurant with a Gemini theme"}]}]}. Optional: add "generationConfig":{"responseModalities":["TEXT","IMAGE"]}.
  - Evidence: https://ai.google.dev/gemini-api/docs/generate-content/image-generation.md.txt lines 205-215, 668-682
- **[verified]** Alternative NEW surface: POST https://generativelanguage.googleapis.com/v1beta/interactions (v1beta only; no v1). Verbatim body: {"model":"gemini-3.1-flash-image","input":[{"type":"text","text":"..."}]}. Response: {"id":..., "output_text":..., "output_image":{"data":"<base64>","mime_type":"image/jpeg"}, "steps":[...]}. Config via "response_format":{"type":"image","mime_type":"image/jpeg","aspect_ratio":"16:9","image_size":"2K"}. Multi-turn via "previous_interaction_id".
  - Evidence: image-generation.md.txt lines 84-95, 308-324, 2436-2450; live POST confirmed path exists (returned API_KEY_INVALID not 404)
- **[verified]** GOTCHA (verified live): /v1beta/interactions returns errors as a top-level JSON ARRAY: [{"error":{"code":400,...}}]. I confirmed via python json.load that TOP-LEVEL TYPE is list. /v1/models/...:generateContent and /v1beta/models return a bare object {"error":{...}}. A serde struct expecting an object will fail to deserialize interactions errors.
  - Evidence: Live curl POST to /v1beta/interactions with bogus key; head -c 3 => '[{'; python3 json.load => TOP-LEVEL TYPE: list
- **[verified]** Image data is returned BASE64 INLINE, never as a URL. generateContent path: candidates[0].content.parts[].inlineData.data (base64) + inlineData.mimeType. Interactions path: output_image.data + output_image.mime_type. Text and image parts are interleaved, so you must iterate parts and pick the one where inlineData is present.
  - Evidence: generate-content/image-generation.md.txt lines 82-83 (part.inlineData.data); image-generation.md.txt lines 56, 97 (output_image.data, base64.b64decode)
- **[verified]** Auth: BOTH work — header `x-goog-api-key: $GEMINI_API_KEY` and query `?key=$GEMINI_API_KEY`. I tested both live against /v1beta/models and got identical API_KEY_INVALID responses. The header is what every current official doc example uses and is preferred (keeps the secret out of URLs, proxy logs, and Referer headers). Docs: 'All requests to the Gemini API must include a x-goog-api-key header with your API key.'
  - Evidence: Live curl both forms; all REST examples in image-generation.md.txt use -H "x-goog-api-key: $GEMINI_API_KEY"
- **[verified]** CHEAP KEY VALIDATION: GET https://generativelanguage.googleapis.com/v1beta/models?pageSize=1 with header x-goog-api-key. Costs zero tokens, no billing. Returns {"models":[{...}],"nextPageToken":string}. Model object fields: name, baseModelId, version, displayName, description, inputTokenLimit, outputTokenLimit, supportedGenerationMethods[], thinking, temperature, maxTemperature, topP, topK. Default pageSize is 50, max 1000 — pass pageSize=1 to keep the response tiny.
  - Evidence: https://ai.google.dev/api/models.md.txt lines 60-90, 175, 226; live GET returns proper error for bad key
- **[verified]** CRITICAL: invalid API key => HTTP 400 with status INVALID_ARGUMENT, NOT 401 or 403. Exact live body: {"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com","metadata":{"service":"generativelanguage.googleapis.com"}},{"@type":"type.googleapis.com/google.rpc.LocalizedMessage","locale":"en-US","message":"API key not valid. Please pass a valid API key."}]}}. Since a malformed request is ALSO 400/INVALID_ARGUMENT, status code alone cannot distinguish them — you MUST check details[].reason == "API_KEY_INVALID".
  - Evidence: Live curl GET /v1beta/models -H 'x-goog-api-key: INVALID_KEY_TEST_123' => HTTP 400, body reproduced verbatim
- **[verified]** Missing key entirely => HTTP 403 PERMISSION_DENIED, body {"error":{"code":403,"message":"Method doesn't allow unregistered callers (callers without established identity). Please use API Key or other form of API consumer identity to call this API.","status":"PERMISSION_DENIED"}} — note NO details[] array on this one, so details must be Option<Vec<_>> in Rust.
  - Evidence: Live curl GET /v1beta/models with no auth header
- **[verified]** Full documented error taxonomy: 400 INVALID_ARGUMENT (malformed body OR invalid key — disambiguate via details.reason); 400 FAILED_PRECONDITION (free tier unavailable in your country, enable billing); 403 PERMISSION_DENIED (wrong key / lacks permission); 404 NOT_FOUND; 429 RESOURCE_EXHAUSTED (exceeded RPM/TPM/RPD/spend); 499 CANCELLED; 500 INTERNAL; 503 UNAVAILABLE (overloaded); 504 DEADLINE_EXCEEDED.
  - Evidence: https://ai.google.dev/gemini-api/docs/troubleshooting.md.txt lines 18-28
- **[verified]** Retry guidance verbatim: 'Only retry on transient errors (like 429, 408, or 5xx). Do not retry on client errors (like 400 or 403) as they indicate issues like invalid API keys or bad syntax.' Use exponential backoff. Reference behavior: the official Python SDK retries transient errors up to four times, initial delay ~1 second, max delay 60 seconds.
  - Evidence: troubleshooting.md.txt lines 32-40
- **[verified]** NO FREE TIER for any image generation model. Every Nano Banana pricing table lists Free Tier = 'Not available' for both input and output price. Consequence: a free-tier key passes models.list validation but fails image generation. Text models DO have a free tier ('Free of charge').
  - Evidence: pricing.md.txt: gemini-3.1-flash-lite-image and gemini-3.1-flash-image tables show 'Not available' under Free Tier; gemini-3.1-flash-lite text shows 'Free of charge'
- **[verified]** Pricing per image (paid tier, standard): gemini-3.1-flash-lite-image $0.0336/1K (batch $0.0168). gemini-3.1-flash-image $0.045 per 0.5K, $0.067 per 1K, $0.101 per 2K, $0.151 per 4K (batch: $0.022/$0.034/$0.050/$0.076). gemini-3-pro-image $0.134 per 1K/2K, $0.24 per 4K (batch/flex $0.067 / $0.12). gemini-2.5-flash-image $0.039/image. Mechanism: images billed as output tokens ($30/1M for lite, $60/1M for flash); a 1K image = 1120 tokens. Batch API = flat 50% discount, 24h turnaround.
  - Evidence: https://ai.google.dev/gemini-api/docs/pricing.md.txt — flash-lite-image and flash-image Standard/Batch tables
- **[verified]** Aspect ratio / size params on generateContent: generationConfig.imageConfig = {"aspectRatio": string, "imageSize": string}. Supported aspectRatio (verbatim from API ref): 1:1, 1:4, 4:1, 1:8, 8:1, 2:3, 3:2, 3:4, 4:3, 4:5, 5:4, 9:16, 16:9, 21:9. Supported imageSize: 512, 1K, 2K, 4K — default 1K if unspecified. 'An error will be returned if this field is set for models that don't support these config options.'
  - Evidence: https://ai.google.dev/api/generate-content.md.txt lines 3381, 3473-3483 (ImageConfig)
- **[verified]** Aspect ratio / size on the Interactions API use snake_case and slightly different size values: response_format.aspect_ratio and response_format.image_size, where image_size is '512px' (0.5K), '1K', '2K', '4K' — note '512px' here vs '512' in generationConfig.imageConfig. Don't copy the value across surfaces.
  - Evidence: image-generation.md.txt lines 2410-2450, 337 ('Gemini 3.1 Flash Image adds the smaller 512px (0.5K) resolution')
- **[verified]** Concrete pixel dims for Atlas: 1:1 => 1024x1024 (1K) / 2048x2048 (2K) / 512x512 (0.5K, flash-image only). 16:9 => 1376x768 (1K). 4:3 => 1200x896 (1K). 3:2 => 1264x848 (1K). Use 1:1 @1K for avatars/icons, 16:9 or 3:2 @1K-2K for project cover art. gemini-3.1-flash-lite-image supports 1K ONLY.
  - Evidence: image-generation.md.txt lines 2457-2487 (3.1 Flash Image and 3.1 Pro Image resolution tables); line 338
- **[verified]** THERE IS NO IMAGE COUNT PARAMETER for Nano Banana. The old Imagen sampleCount (1-4, default 4) is going away with Imagen. Docs warn: 'The model won't always follow the exact number of image outputs that the user explicitly asks for.' To generate N candidate avatars/covers, issue N separate requests (parallelizable) rather than asking for N in one prompt.
  - Evidence: image-generation.md.txt line 2355 (Limitations); imagen.md.txt sampleCount 1-4 default 4 (deprecated path)
- **[verified]** Safety fields to handle: promptFeedback.blockReason (prompt rejected, no candidates returned) with enum values SAFETY, BLOCKLIST, PROHIBITED_CONTENT, IMAGE_SAFETY; and candidates[].finishReason with image-specific values IMAGE_SAFETY ('Token generation stopped because generated images contain safety violations') and IMAGE_PROHIBITED_CONTENT, plus SAFETY, BLOCKLIST, PROHIBITED_CONTENT, MAX_TOKENS, STOP. promptFeedback also carries safetyRatings[]. A 200 OK can therefore contain ZERO images.
  - Evidence: https://ai.google.dev/api/generate-content.md.txt lines 2796-2814 (BlockReason), 2952-2961 (FinishReason)
- **[verified]** Custom safety settings are NOT supported in the Interactions API (verbatim: 'Custom safety settings are not supported in the Interactions API.'). If Atlas ever needs to tune safety thresholds, that is another reason to use generateContent.
  - Evidence: https://ai.google.dev/gemini-api/docs/interactions-overview.md.txt line 162
- **[verified]** ALL generated images carry an invisible SynthID watermark — stated twice in the docs, no opt-out. Relevant if Atlas ever claims images are user-owned/original.
  - Evidence: image-generation.md.txt lines 37, 2359
- **[verified]** Text models for auto-generating card descriptions: `gemini-3.1-flash-lite` (most cost-efficient, 'optimized for high-volume agentic tasks, translation, and simple data processing' — free tier: Free of charge; paid $0.25/1M in, $1.50/1M out) and `gemini-3.5-flash` (current default flash). Also stable: gemini-2.5-flash, gemini-2.5-flash-lite, gemini-2.5-pro. Preview: gemini-3.1-pro-preview, gemini-3-flash-preview. Same endpoint shape: POST /v1/models/gemini-3.1-flash-lite:generateContent.
  - Evidence: https://ai.google.dev/gemini-api/docs/models.md.txt lines 11-61; pricing.md.txt lines 117-140
- **[verified]** Structured output for card descriptions is supported via generationConfig.responseMimeType + responseSchema (or responseJsonSchema) — lets Atlas get typed JSON back instead of parsing prose.
  - Evidence: https://ai.google.dev/api/generate-content.md.txt line 3387 (GenerationConfig JSON representation)
- *[likely]* REST JSON accepts BOTH camelCase and snake_case for field names (proto3 JSON mapping) — the docs' own curl examples mix them, e.g. "inline_data"/"mime_type" in one example and inlineData/mimeType in the reference. Responses come back camelCase. Serde structs should be #[serde(rename_all = "camelCase")] for decoding.
  - Evidence: generate-content/image-generation.md.txt lines 446-458 uses \"inline_data\"/\"mime_type\"; api/generate-content.md.txt reference uses inlineData/mimeType
- *[uncertain]* gemini-2.5-flash-image (legacy Nano Banana) is reported to shut down October 2, 2026. I could not confirm this date in the raw docs I fetched (only the 'strongly recommend transition' language), so treat the exact date as unconfirmed — but do not build new code on 2.5-flash-image regardless.
  - Evidence: Search result summary (secondary sources); NOT found in ai.google.dev raw docs I fetched
- **[verified]** Rate limits: measured in RPM, TPM, RPD; image models use Images Per Minute (IPM) instead of TPM. Tiers: Free, Tier 1 (billing enabled), Tier 2 ($100+ spent, 3 days), Tier 3 ($1,000+ spent, 30 days). Docs decline to publish exact per-model numbers: 'specified rate limits are not guaranteed and actual capacity may vary' — check AI Studio. Exceeding => 429 RESOURCE_EXHAUSTED.
  - Evidence: https://ai.google.dev/gemini-api/docs/rate-limits.md.txt line 24 and tier tables

## Risks

- Imagen shuts down August 17, 2026 — 32 days from today (2026-07-16). Any implementation targeting imagen-4.0-* :predict will break almost immediately. This is the single biggest trap in the original task framing, which assumed Imagen was a live option.
- Invalid key returns HTTP 400/INVALID_ARGUMENT, not 401/403. Any implementation that classifies auth failures by status code will misreport an expired key as a malformed request (and vice versa — a genuinely malformed body will be reported to the user as 'your API key is invalid'). Must branch on error.details[].reason == API_KEY_INVALID.
- Image generation has NO free tier. models.list validation returns 200 for a free-tier key, so Atlas can tell a user 'key OK' and then fail every image generation with 429. 429 here is permanent, not transient — retrying with backoff will burn time and never succeed. Distinguish 'no paid quota' from 'rate limited' if possible; the API does not make this easy, so consider surfacing billing guidance on repeated 429s.
- /v1beta/interactions array-wraps its error bodies ([{"error":...}]) while /v1/...:generateContent does not. Mixing surfaces means two error decoders. Recommendation is to use generateContent only.
- The 403 no-key error body has NO details[] field; the 400 invalid-key body does. serde will fail on a non-Option/non-default details field. Use #[serde(default)].
- image_size values differ across surfaces: '512px' on Interactions vs '512' in generationConfig.imageConfig. Copying a value across surfaces yields an error.
- gemini-3.1-flash-lite-image supports 1K resolution ONLY — requesting 2K/4K will error ('An error will be returned if this field is set for models that don't support these config options'). If Atlas exposes a size selector, gate it per model.
- Google does not publish exact per-model rate limits ('not guaranteed and actual capacity may vary'), so Atlas cannot hardcode a client-side limiter safely — it must react to 429s rather than predict them.
- All generated images carry an invisible SynthID watermark with no opt-out — relevant if Atlas markets generated cover art as fully user-owned/original.
- A 200 OK response can legitimately contain zero images (safety block). Code that assumes parts[0].inlineData exists will panic on blocked prompts — a likely occurrence for user-supplied avatar prompts involving real people.
- gemini-2.5-flash-image reportedly shuts down Oct 2, 2026, but I could not confirm that date in official raw docs — verify before relying on the legacy model for any migration window.
- Model IDs churn fast in this family (2.5 -> 3 Pro -> 3.1 Flash within ~a year). Put the model ID in config, not inline in code, so Atlas can roll forward without a release.

# Comparing catalog models

The picker offers **Selection tier**, a NodeCraft editorial priority for automatic selection:
**1 is lowest; 5 is highest**. The stored field is still `quality` for compatibility. It has no
benchmark, dataset, scoring formula or measured accuracy behind it. Equal tiers do not imply
equal image fidelity, reasoning ability or suitability across tasks or providers.

The existing integers are retained. They order compatible choices and contribute to the
Fast, Balanced and Best selection policies in [`catalog.md`](catalog.md). Best prioritizes this
tier; Fast prioritizes the configured time hint; Balanced subtracts a wait penalty from the tier.
A tier is not a provider recommendation or a promise about an individual result.

**Time hint** is the catalog's configured `latency_s` estimate, not a timed experiment, percentile
or service guarantee. Request size, settings, hardware and provider load affect the actual wait.
The price line separately describes the catalog rate: per-call and other quoted rates are
estimates; usage pricing may have no reliable up-front total. Actual run quotes, account credits
and budgets are checked by the execution path. This wording change does not change prices,
selection order, supported modes or defaults.

Descriptions state the purpose and differences of the model **as exposed in NodeCraft**. For
example, OpenAI describes Flare as its faster everyday image option and Sunburst as focused on
quality and detailed edits. The picker expresses those purposes without promising a 50% latency
improvement for every request. Both retain their existing selection tier. Azure deployments and
imported ComfyUI workflows cannot have a universal speed or quality claim because the user's
chosen deployment, nodes and server determine them.

The audit below was checked against primary documentation on **2026-10-27**. It records the
source for capability wording, not benchmark evidence for a numeric tier. Provider documentation
can describe features beyond NodeCraft's supported subset; supported modes, local limits,
pricing units and uncertainty notices also come from the shipped catalog and tested adapters.
No paid generation was used for this audit. Descriptions are original summaries, not copied
provider marketing text.

Retirement notices are scoped to the service: the Google Cloud Nano Banana date comes from its
Cloud model page; Gemini API notices must not be applied to Google Cloud Text-to-Speech.
OpenAI shutdown dates come from its [deprecations page](https://developers.openai.com/api/docs/deprecations).
The Luma download limit is NodeCraft's own 256 MiB per-result bound, not a vendor limit.

| Model id | Primary capability source | Checked |
| --- | --- | --- |
| `anthropic.claude-haiku-5-5` | [Documentation](https://platform.claude.com/docs/en/models/overview) | 2026-10-27 |
| `anthropic.claude-opus-5-5` | [Documentation](https://platform.claude.com/docs/en/models/overview) | 2026-10-27 |
| `anthropic.claude-sonnet-5-5` | [Documentation](https://platform.claude.com/docs/en/models/overview) | 2026-10-27 |
| `azure-openai.deployment` | [Documentation](https://learn.microsoft.com/en-us/azure/ai-studio/ai-services/concepts/endpoints) | 2026-10-27 |
| `bfl.flux-2-klein-4b` | [Documentation](https://docs.bfl.ai/quick_start/generating_images) | 2026-10-27 |
| `comfyui.flux-dev` | [Documentation](https://docs.comfy.org/tutorials/flux/flux-1-text-to-image) | 2026-10-27 |
| `comfyui.sdxl` | [Documentation](https://huggingface.co/stabilityai/stable-diffusion-xl-base-1.0) | 2026-10-27 |
| `comfyui.workflow` | [Documentation](https://docs.comfy.org/tutorials/flux/flux-1-text-to-image) | 2026-10-27 |
| `deepgram.nova-3` | [Documentation](https://developers.deepgram.com/docs/pre-recorded-audio) | 2026-10-27 |
| `elevenlabs.flash-v2-5` | [Documentation](https://elevenlabs.io/docs/overview/models) | 2026-10-27 |
| `elevenlabs.multilingual-v2` | [Documentation](https://elevenlabs.io/docs/overview/models) | 2026-10-27 |
| `elevenlabs.music` | [Documentation](https://elevenlabs.io/docs/api-reference/music/compose) | 2026-10-27 |
| `elevenlabs.sound-effects` | [Documentation](https://elevenlabs.io/docs/api-reference/text-to-sound-effects/convert) | 2026-10-27 |
| `elevenlabs.v3` | [Documentation](https://elevenlabs.io/docs/overview/models) | 2026-10-27 |
| `elevenlabs.voice-changer` | [Documentation](https://elevenlabs.io/docs/api-reference/speech-to-speech/convert) | 2026-10-27 |
| `fal.bria-rmbg` | [Documentation](https://fal.ai/models/fal-ai/bria/background/remove) | 2026-10-27 |
| `fal.esrgan` | [Documentation](https://fal.ai/models/fal-ai/esrgan) | 2026-10-27 |
| `fal.flux-lora` | [Documentation](https://fal.ai/models/fal-ai/flux-lora/api) | 2026-10-27 |
| `fal.flux-lora-trainer` | [Documentation](https://fal.ai/models/fal-ai/flux-lora-fast-training/api) | 2026-10-27 |
| `fal.flux-schnell` | [Documentation](https://fal.ai/models/fal-ai/flux/schnell) | 2026-10-27 |
| `fal.ideogram-v3` | [Documentation](https://fal.ai/models/fal-ai/ideogram/v3) | 2026-10-27 |
| `fal.kokoro` | [Documentation](https://fal.ai/models/fal-ai/kokoro/american-english) | 2026-10-27 |
| `fal.grok-image2` | [Documentation](https://fal.ai/models/xai/grok-imagine-image/v2.0/text-to-image/api) | 2026-10-27 |
| `fal.grok-image2-edit` | [Documentation](https://fal.ai/models/xai/grok-imagine-image/v2.0/edit/api) | 2026-10-27 |
| `fal.grok-video15` | [Documentation](https://fal.ai/models/xai/grok-imagine-video/v1.5/text-to-video/api) | 2026-10-27 |
| `fal.grok-video15-i2v` | [Documentation](https://fal.ai/models/xai/grok-imagine-video/v1.5/image-to-video/api) | 2026-10-27 |
| `fal.ltx23` | [Documentation](https://fal.ai/models/fal-ai/ltx-2.3/text-to-video/api) | 2026-10-27 |
| `fal.ltx23-i2v` | [Documentation](https://fal.ai/models/fal-ai/ltx-2.3/image-to-video/api) | 2026-10-27 |
| `fal.luma-photon` | [Documentation](https://fal.ai/models/fal-ai/luma-photon) | 2026-10-27 |
| `fal.luma-ray32` | [Documentation](https://fal.ai/models/luma/agent/ray/v3.2/text-to-video) | 2026-10-27 |
| `fal.luma-ray32-i2v` | [Documentation](https://fal.ai/models/luma/agent/ray/v3.2/image-to-video/api) | 2026-10-27 |
| `fal.trellis2` | [Documentation](https://fal.ai/models/fal-ai/trellis-2) | 2026-10-27 |
| `fal.wan22` | [Documentation](https://fal.ai/models/fal-ai/wan/v2.2-a14b/text-to-video) | 2026-10-27 |
| `fal.wan22-i2v` | [Documentation](https://fal.ai/models/fal-ai/wan/v2.2-a14b/image-to-video) | 2026-10-27 |
| `fal.wan22-s2v` | [Documentation](https://fal.ai/models/fal-ai/wan/v2.2-14b/speech-to-video) | 2026-10-27 |
| `gemini-images.flash-lite` | [Documentation](https://ai.google.dev/gemini-api/docs/image-generation) | 2026-10-27 |
| `gemini-images.nano-banana-2.1` | [Documentation](https://ai.google.dev/gemini-api/docs/image-generation) | 2026-10-27 |
| `gemini-images.pro` | [Documentation](https://ai.google.dev/gemini-api/docs/image-generation) | 2026-10-27 |
| `gemini-llm.3.8-flash` | [Documentation](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash) | 2026-10-27 |
| `gemini-video.omni-flash` | [Documentation](https://ai.google.dev/gemini-api/docs/omni) | 2026-10-27 |
| `google-gemini-images.2.5-flash` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/2-5-flash-image) | 2026-10-27 |
| `google-gemini-images.3-pro` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/image-generation) | 2026-10-27 |
| `google-gemini-images.3.1-flash` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/image-generation) | 2026-10-27 |
| `google-gemini-images.3.1-flash-lite` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/image-generation) | 2026-10-27 |
| `google-gemini-images.nano-banana-2.1` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/image-generation) | 2026-10-27 |
| `google-tts.chirp3-hd` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/list-voices-and-types) | 2026-10-27 |
| `google-tts.gemini-2.5-flash` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/gemini-tts) | 2026-10-27 |
| `google-tts.gemini-2.5-flash-lite` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/gemini-tts) | 2026-10-27 |
| `google-tts.gemini-2.5-pro` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/gemini-tts) | 2026-10-27 |
| `google-tts.gemini-3.1-flash` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/gemini-tts) | 2026-10-27 |
| `google-tts.neural2` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/list-voices-and-types) | 2026-10-27 |
| `google-tts.standard` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/list-voices-and-types) | 2026-10-27 |
| `google-tts.studio` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/list-voices-and-types) | 2026-10-27 |
| `google-tts.wavenet` | [Documentation](https://docs.cloud.google.com/text-to-speech/docs/list-voices-and-types) | 2026-10-27 |
| `google-veo.veo-3.1` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/veo/3-1-generate) | 2026-10-27 |
| `google-veo.veo-3.1-fast` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/veo/3-1-generate) | 2026-10-27 |
| `google-veo.veo-3.1-lite` | [Documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/veo/3-1-generate) | 2026-10-27 |
| `higgsfield.genjutsu-motion-transfer` | [Documentation](https://docs.higgsfield.ai/docs/models/genjutsu/motion-transfer) | 2026-10-27 |
| `higgsfield.genjutsu-object-swap` | [Documentation](https://docs.higgsfield.ai/docs/models/genjutsu/object-swap) | 2026-10-27 |
| `higgsfield.genjutsu-restyle` | [Documentation](https://docs.higgsfield.ai/docs/models/genjutsu/restyle) | 2026-10-27 |
| `higgsfield.kling-2.5-turbo-i2v` | [Documentation](https://docs.higgsfield.ai/docs) | 2026-10-27 |
| `higgsfield.soul-2` | [Documentation](https://higgsfield.ai/soul-intro) | 2026-10-27 |
| `higgsfield.soul-id-trainer` | [Documentation](https://open.higgsfield.ai/models/soul-id/api-reference) | 2026-10-27 |
| `huggingface.gpt-oss-120b-groq` | [Documentation](https://huggingface.co/docs/inference-providers/en/guides/gpt-oss) | 2026-10-27 |
| `kling.image_v2_1` | [Documentation](https://kling.ai/document-api/updates/api?date=2026-02-11) | 2026-10-27 |
| `kling.v1_6` | [Documentation](https://kling.ai/document-api/updates/api?date=2026-02-11) | 2026-10-27 |
| `kling.v2_1_master` | [Documentation](https://kling.ai/document-api/updates/api?date=2026-02-11) | 2026-10-27 |
| `kling.v2_5_turbo` | [Documentation](https://kling.ai/document-api/updates/api?date=2026-02-11) | 2026-10-27 |
| `meshy.image-to-3d` | [Documentation](https://docs.meshy.ai/en/api) | 2026-10-27 |
| `meshy.multi-image-to-3d` | [Documentation](https://docs.meshy.ai/en/api) | 2026-10-27 |
| `meshy.remesh` | [Documentation](https://docs.meshy.ai/en/api) | 2026-10-27 |
| `meshy.retexture` | [Documentation](https://docs.meshy.ai/en/api/retexture) | 2026-10-27 |
| `meshy.retexture-image` | [Documentation](https://docs.meshy.ai/en/api/retexture) | 2026-10-27 |
| `meshy.text-to-3d` | [Documentation](https://docs.meshy.ai/en/api/text-to-3d) | 2026-10-27 |
| `openai-audio.mini-tts` | [Documentation](https://developers.openai.com/api/docs/deprecations) | 2026-10-27 |
| `openai-audio.transcribe` | [Documentation](https://developers.openai.com/api/docs/models/gpt-transcribe) | 2026-10-27 |
| `openai-images.flare` | [Documentation](https://developers.openai.com/api/docs/guides/image-prompting) | 2026-10-27 |
| `openai-images.sunburst` | [Documentation](https://developers.openai.com/api/docs/guides/image-prompting) | 2026-10-27 |
| `openai.gpt-4.1-mini` | [Documentation](https://developers.openai.com/api/docs/models/gpt-4.1-mini) | 2026-10-27 |
| `openai.gpt-6-luna` | [Documentation](https://developers.openai.com/api/docs/models/gpt-6-luna) | 2026-10-27 |
| `openai.gpt-6.1-sol` | [Documentation](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | 2026-10-27 |
| `replicate.hello` | [Documentation](https://replicate.com/replicate/hello-world/versions/5c7d5dc6dd8bf75c1acaa8565735e7986bc5b66206b55cca93cb72c9bf15ccaa/api) | 2026-10-27 |
| `replicate.sdxl` | [Documentation](https://replicate.com/stability-ai/sdxl/versions/7762fd07cf82c948538e41f63f77d685e02b063e37e496e96eefd46c929f9bdc/api) | 2026-10-27 |
| `runway.gen4-5` | [Documentation](https://docs.dev.runwayml.com/guides/models/) | 2026-10-27 |
| `runway.gen4-turbo` | [Documentation](https://docs.dev.runwayml.com/guides/models/) | 2026-10-27 |
| `seedance.2-0` | [Documentation](https://docs.byteplus.com/en/docs/byteplus_las/video_gen_enhanced) | 2026-10-27 |
| `seedance.2-0-fast` | [Documentation](https://docs.byteplus.com/en/docs/byteplus_las/video_gen_enhanced) | 2026-10-27 |
| `seedance.2-0-mini` | [Documentation](https://docs.byteplus.com/en/docs/byteplus_las/video_gen_enhanced) | 2026-10-27 |
| `seedance.2-5` | [Documentation](https://docs.byteplus.com/en/docs/byteplus_las/video_gen_enhanced) | 2026-10-27 |
| `seedream.4_5` | [Documentation](https://docs.byteplus.com/en/docs/modelark/seedream-4-0-5-0?redirect=1) | 2026-10-27 |
| `tripo.image-to-3d` | [Documentation](https://docs.tripo3d.ai/model-generation/text-to-model-v3-0-v3-1.html) | 2026-10-27 |
| `tripo.multiview-to-3d` | [Documentation](https://docs.tripo3d.ai/model-generation/text-to-model-v3-0-v3-1.html) | 2026-10-27 |
| `tripo.text-to-3d` | [Documentation](https://docs.tripo3d.ai/model-generation/text-to-model-v3-0-v3-1.html) | 2026-10-27 |

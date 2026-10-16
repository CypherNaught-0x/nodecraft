//! Hand-written test fixtures in ComfyUI's public JSON formats (no files copied from ComfyUI).
//! `API_WORKFLOW` and `UI_WORKFLOW` describe the same txt2img + LoRA graph; the UI one adds the
//! editor-only things a converter must handle: a note, a reroute, a primitive feeding the seed,
//! a muted output and a bypassed node of a class the server doesn't have.

pub const OBJECT_INFO: &str = r#"{
  "CheckpointLoaderSimple": {
    "input": {"required": {"ckpt_name": [["sd_xl_base_1.0.safetensors", "flux1-dev.safetensors"], {}]}},
    "output": ["MODEL", "CLIP", "VAE"], "output_is_list": [false, false, false], "output_name": ["MODEL", "CLIP", "VAE"],
    "name": "CheckpointLoaderSimple", "display_name": "Load Checkpoint", "category": "loaders", "output_node": false
  },
  "LoraLoader": {
    "input": {"required": {
      "model": ["MODEL", {}], "clip": ["CLIP", {}],
      "lora_name": [["detail.safetensors", "style.safetensors"], {}],
      "strength_model": ["FLOAT", {"default": 1.0, "min": -100.0, "max": 100.0, "step": 0.01}],
      "strength_clip": ["FLOAT", {"default": 1.0, "min": -100.0, "max": 100.0, "step": 0.01}]
    }},
    "output": ["MODEL", "CLIP"], "output_name": ["MODEL", "CLIP"], "display_name": "Load LoRA", "category": "loaders"
  },
  "CLIPTextEncode": {
    "input": {"required": {"text": ["STRING", {"multiline": true, "dynamicPrompts": true}], "clip": ["CLIP", {}]}},
    "output": ["CONDITIONING"], "output_name": ["CONDITIONING"], "display_name": "CLIP Text Encode (Prompt)", "category": "conditioning"
  },
  "EmptyLatentImage": {
    "input": {"required": {
      "width": ["INT", {"default": 1024, "min": 16, "max": 16384, "step": 8}],
      "height": ["INT", {"default": 1024, "min": 16, "max": 16384, "step": 8}],
      "batch_size": ["INT", {"default": 1, "min": 1, "max": 4096}]
    }},
    "output": ["LATENT"], "display_name": "Empty Latent Image", "category": "latent"
  },
  "KSampler": {
    "input": {"required": {
      "model": ["MODEL", {}],
      "seed": ["INT", {"default": 0, "min": 0, "max": 18446744073709551615, "control_after_generate": true}],
      "steps": ["INT", {"default": 20, "min": 1, "max": 10000}],
      "cfg": ["FLOAT", {"default": 8.0, "min": 0.0, "max": 100.0, "step": 0.1}],
      "sampler_name": [["euler", "euler_ancestral", "dpmpp_2m"], {}],
      "scheduler": [["normal", "karras"], {}],
      "positive": ["CONDITIONING", {}], "negative": ["CONDITIONING", {}], "latent_image": ["LATENT", {}],
      "denoise": ["FLOAT", {"default": 1.0, "min": 0.0, "max": 1.0, "step": 0.01}]
    }},
    "output": ["LATENT"], "output_name": ["LATENT"], "display_name": "KSampler", "category": "sampling"
  },
  "VAEDecode": {
    "input": {"required": {"samples": ["LATENT", {}], "vae": ["VAE", {}]}},
    "output": ["IMAGE"], "display_name": "VAE Decode", "category": "latent"
  },
  "SaveImage": {
    "input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {"default": "ComfyUI"}]}},
    "output": [], "display_name": "Save Image", "category": "image", "output_node": true
  },
  "PreviewImage": {
    "input": {"required": {"images": ["IMAGE", {}]}},
    "output": [], "display_name": "Preview Image", "category": "image", "output_node": true
  },
  "LoadImage": {
    "input": {"required": {"image": [["example.png"], {"image_upload": true}]}},
    "output": ["IMAGE", "MASK"], "display_name": "Load Image", "category": "image"
  },
  "BrokenCustomNode": 5
}"#;

pub const API_WORKFLOW: &str = r#"{
  "3": {"class_type": "KSampler", "inputs": {
    "seed": 42, "steps": 20, "cfg": 7.5, "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0,
    "model": ["10", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]}},
  "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sd_xl_base_1.0.safetensors"}},
  "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 768, "batch_size": 1}},
  "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat in a spacesuit", "clip": ["10", 1]}, "_meta": {"title": "Positive"}},
  "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["10", 1]}},
  "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
  "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "NodeCraft", "images": ["8", 0]}},
  "10": {"class_type": "LoraLoader", "inputs": {"model": ["4", 0], "clip": ["4", 1], "lora_name": "detail.safetensors", "strength_model": 0.8, "strength_clip": 1.0}}
}"#;

pub const UI_WORKFLOW: &str = r#"{
  "last_node_id": 15, "last_link_id": 15, "version": 0.4,
  "nodes": [
    {"id": 4, "type": "CheckpointLoaderSimple", "mode": 0, "inputs": [],
     "outputs": [{"name": "MODEL", "type": "MODEL", "links": [1]}, {"name": "CLIP", "type": "CLIP", "links": [2]}, {"name": "VAE", "type": "VAE", "links": [11]}],
     "widgets_values": ["sd_xl_base_1.0.safetensors"]},
    {"id": 10, "type": "LoraLoader", "mode": 0,
     "inputs": [{"name": "model", "type": "MODEL", "link": 1}, {"name": "clip", "type": "CLIP", "link": 2}],
     "outputs": [{"name": "MODEL", "type": "MODEL", "links": [3]}, {"name": "CLIP", "type": "CLIP", "links": [4, 5]}],
     "widgets_values": ["detail.safetensors", 0.8, 1.0]},
    {"id": 6, "type": "CLIPTextEncode", "mode": 0, "title": "Positive",
     "inputs": [{"name": "clip", "type": "CLIP", "link": 4}],
     "outputs": [{"name": "CONDITIONING", "type": "CONDITIONING", "links": [6]}],
     "widgets_values": ["a cat in a spacesuit"]},
    {"id": 7, "type": "CLIPTextEncode", "mode": 0,
     "inputs": [{"name": "clip", "type": "CLIP", "link": 5}],
     "outputs": [{"name": "CONDITIONING", "type": "CONDITIONING", "links": [7]}],
     "widgets_values": ["blurry"]},
    {"id": 5, "type": "EmptyLatentImage", "mode": 0, "inputs": [],
     "outputs": [{"name": "LATENT", "type": "LATENT", "links": [8]}],
     "widgets_values": [1024, 768, 1]},
    {"id": 15, "type": "SomeUninstalledUpscale", "mode": 4,
     "inputs": [{"name": "samples", "type": "LATENT", "link": 8}],
     "outputs": [{"name": "LATENT", "type": "LATENT", "links": [9]}],
     "widgets_values": ["nearest", 2.0]},
    {"id": 13, "type": "PrimitiveNode", "mode": 0, "inputs": [],
     "outputs": [{"name": "INT", "type": "INT", "links": [14], "widget": {"name": "seed"}}],
     "widgets_values": [42, "fixed"]},
    {"id": 3, "type": "KSampler", "mode": 0,
     "inputs": [{"name": "model", "type": "MODEL", "link": 3}, {"name": "positive", "type": "CONDITIONING", "link": 6},
                {"name": "negative", "type": "CONDITIONING", "link": 7}, {"name": "latent_image", "type": "LATENT", "link": 9},
                {"name": "seed", "type": "INT", "link": 14, "widget": {"name": "seed"}}],
     "outputs": [{"name": "LATENT", "type": "LATENT", "links": [10]}],
     "widgets_values": [7, "randomize", 20, 7.5, "euler", "normal", 1.0]},
    {"id": 12, "type": "Reroute", "mode": 0,
     "inputs": [{"name": "", "type": "*", "link": 11}], "outputs": [{"name": "", "type": "VAE", "links": [12]}]},
    {"id": 8, "type": "VAEDecode", "mode": 0,
     "inputs": [{"name": "samples", "type": "LATENT", "link": 10}, {"name": "vae", "type": "VAE", "link": 12}],
     "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [13, 15]}], "widgets_values": []},
    {"id": 9, "type": "SaveImage", "mode": 0,
     "inputs": [{"name": "images", "type": "IMAGE", "link": 13}], "outputs": [], "widgets_values": ["NodeCraft"]},
    {"id": 14, "type": "SaveImage", "mode": 2,
     "inputs": [{"name": "images", "type": "IMAGE", "link": 15}], "outputs": [], "widgets_values": ["muted"]},
    {"id": 11, "type": "Note", "mode": 0, "inputs": [], "outputs": [], "widgets_values": ["Remember to pick a checkpoint."]}
  ],
  "links": [
    [1, 4, 0, 10, 0, "MODEL"], [2, 4, 1, 10, 1, "CLIP"], [3, 10, 0, 3, 0, "MODEL"], [4, 10, 1, 6, 0, "CLIP"],
    [5, 10, 1, 7, 0, "CLIP"], [6, 6, 0, 3, 1, "CONDITIONING"], [7, 7, 0, 3, 2, "CONDITIONING"], [8, 5, 0, 15, 0, "LATENT"],
    [9, 15, 0, 3, 3, "LATENT"], [10, 3, 0, 8, 0, "LATENT"], [11, 4, 2, 12, 0, "VAE"], [12, 12, 0, 8, 1, "VAE"],
    [13, 8, 0, 9, 0, "IMAGE"], [14, 13, 0, 3, 4, "INT"], [15, 8, 0, 14, 0, "IMAGE"]
  ],
  "groups": [], "config": {}, "extra": {}
}"#;

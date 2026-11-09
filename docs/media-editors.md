# Media editors and reusable references

The Layer Editor, Mask/Inpaint and Elements work in the desktop canvas. Their settings, wires
and ordinary edits travel with the project and use its undo history. Generation still needs a
compatible model and provider setup; making an editing card does not submit a paid request.
See [provider capabilities](providers.md) for each model's inputs, price and host limits.

## Composite and re-render

1. Add a **Layer Editor** card and wire image outputs into its image-list input.
2. Open the card's context menu and choose **Edit layers…**. Move, resize or rotate images;
   add text and shapes; use the layer list for visibility, locking, ordering and names.
   A finished gesture is one undo step. Frame dimensions are bounded to 100–4096 pixels.
3. Text and shapes use the same compositor in the editor and the output. **Add font file…**
   imports an individual TrueType or OpenType font on desktop. Fonts belong to the card, so
   they accompany copies and published Techniques. Missing fonts draw with Lato and say so;
   collections and bitmap-only fonts are refused. A card can hold 16 fonts, each up to 16 MiB.
4. Close the editor and use its composited image as the input to another action or a compatible
   Generate image card. Review that card's quote and provider requirements before running it.

Local compositing is available without a provider. The browser can use fonts already saved on
an editor card, but it has no custom-font file picker. The core composition and downstream
planning paths have deterministic tests; this guide does not claim a paid live generation
check for every image model.

## Paint an edit or expand the canvas

Open a ready image card's context menu, then **Image tools → Inpaint…**. This creates Expand
and Mask cards and opens the mask editor. Paint strokes over the region to change, or add
margins to expand the canvas. Enter a prompt, choose a compatible mask model and select
**Create edit**. That adds and wires a generation card; run it separately after checking its
quote. Remove background, upscale, split, colour grade and other Image tools similarly add
their supported cards, with an explanation when a tool is unavailable.

Masks record the content hash and dimensions of the image they were painted over. Replacing
that image with one of the same shape asks you to check the strokes. An aspect-ratio change
of more than 1% blocks a subsequent run until you explicitly choose **Keep strokes** or
**Fit to new image**. Older masks without an image binding retain their original behaviour.
Transparent expanded margins can be included in the edit without painting each new pixel.

## Reuse an Element

Select image cards and choose **Make Element from selection** from a card's context menu.
An Element accepts up to eight ordered reference images, a name, a category and guidelines.
Wire its reference output to a compatible image-list input, or mention its name as `#name`
in a generation card's own prompt. A mention feeds its references after explicit image wires;
it requires a model with a multi-image reference input and does not fill a single init-image
input. A prompt supplied by a wire replaces the card's typed prompt and its mentions.

An Element can use a generated reference sheet in place of its raw images. Its sheet is an
ordinary generation card with normal history and cost confirmation; until that card has a
result, the Element uses its original images. Renaming or removing an Element makes its old
mentions plain text, so update those prompts when changing names.

Choose **Save to Elements library** from the Element's card menu to keep its settings and
images for other desktop projects. The top-bar **Elements** panel can place a copy, rename
an entry, mark it Shared, export an `.ncelement` bundle or import one. A placed copy does not
change when the library entry changes: use **Update from Elements library** to adopt an update.
Removing a library entry leaves already placed projects intact. The Shared flag records intent;
use an exported bundle to transfer an Element rather than assuming it synchronizes a team library.
The browser and headless hosts keep an Elements library only for the session.

## Provider-trained identities are separate

An Element supplies reference pictures. A Higgsfield Soul ID is trained and stored by the
provider account; it is not a portable image, Element or LoRA file. A Train style card can train
one from its pictures, and the Library's Styles tab identifies its provider and account-bound
status. **Use on selected card** sets a compatible SOUL 2 card's identity rather than wiring
it as an image. Account readiness is checked before generation is paid; sharing or opening a
project on another account preserves the setting but does not grant access to that identity.

**Higgsfield SOUL 2 image to image** also requires exactly one scene image, even when a Soul ID
is set. Training and generation are separately quoted, training cannot be cancelled, and an
unknown training submission outcome is not blindly resubmitted. See
[the Higgsfield contract](providers.md#higgsfield-requests) for the current model and account limits.

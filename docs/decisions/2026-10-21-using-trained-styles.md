## 2026-10-21 · Using a trained style: list, use, or start a card with it (#167)

- **Three commands.** `style.list` names the canvas's Train style cards with their family,
  trigger and whether they are trained; given `for`, it also says whether that card can take each
  one and why not. `style.use` wires a trained style into a card's style input, replacing
  whatever was wired there, in one undo step. `style.card` adds an image card on a model that
  takes the style, wires the style in and seeds the prompt with the trigger word, also in one
  step. The canvas menus dispatch the same commands: a Train style card offers "New image card
  with this style", and a card that takes styles has a "Use style" submenu.
- **Why `style.card` exists.** Switching a card's model checks that its wiring fits a mode of
  the new model. A model whose only mode is "styled" needs a style wired before it fits, and a
  card can't take a style before it is on such a model. Rather than loosening the swap check for
  one case, the style starts the card: the card is created on the styled model already wired.
- **Refusals name the way forward.** A style trained for another family, or a model with no style
  input, is refused with the family and up to five catalog models that do take it. An untrained
  card is refused until it has run.

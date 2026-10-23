# Example plugin packages

Two small packages to start a plugin from; [docs/plugin-authoring.md](../../docs/plugin-authoring.md)
walks through them.

- `palette/`: a **data** package. It registers one catalog entry, a colour palette.
- `shout/`: a **text actions** package. It registers one action that upper-cases its text.

Build and check one:

```sh
nodecraft-cli plugin pack examples/plugins/shout --out shout.ncraft-plugin
nodecraft-cli plugin lint shout.ncraft-plugin
```

Both are dedicated to the public domain (CC0 1.0); copy them freely.

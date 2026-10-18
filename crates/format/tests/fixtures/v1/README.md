# Frozen v1 migration fixtures

Generated exclusively from exact reviewed source commit
`08586e3319f5577f1ac6f618103bfa4702e39043`, before expanding the graph model.
The archive is version 1, epoch 7; the recovery stream starts with `NCRJ0001` and
contains three frames: move/edit root, remove node, restore node using its actual inverse.
The expected recovered JSON is the old graph representation (no config/scalars/thread fields).
These bytes must not be regenerated using the v2 writer.

SHA-256:

- comments-v1.ncraft: `4acb486469b15d9cd8ad5e3dd3f7560828a8db659a5a6f7f5f53d959925fe2f0`
- comments-v1.ncrj: `04060fc6fc2662e766d96000c103c5925dd2890e04cde8e73a5e1aad0b8bc4c5`
- recovered-v1-project.json: `a6d59d6bb27b97d20488c3576ee0a09c35ecaad7df9dcc25b4ff6b806a381df2`

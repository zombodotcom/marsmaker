# Asset credits

## astronaut.glb

- **What:** "Astronaut", a rigged and animated low-poly space-suit character. MarsScape uses it for
  every player and colonist. It is the SpaceSuit outfit from the Ultimate Modular Men pack.
- **Author:** Quaternius (https://quaternius.com)
- **Source:** https://poly.pizza/m/3hC2i0CTuO
- **License:** CC0 1.0 Universal (public domain dedication),
  https://creativecommons.org/publicdomain/zero/1.0/. Attribution is not required, but we give it
  anyway.
- **Changes:**
  - Kept only the 9 animations the game plays, in this order: Idle, Idle_Gun, Idle_Gun_Pointing,
    Idle_Gun_Shoot, Run, HitRecieve, Death, Interact, Sword_Slash.
  - Removed the unused animation data and repacked the binary chunk (1.94 MB down to 1.53 MB).
  - Gave every clip a channel for every animated bone. Where a clip had none, that channel holds
    the rest pose. The export had left those channels out, and Bevy would then keep the previous
    clip's pose on them (the IK feet stayed behind after running).
  - Geometry, skeleton and skin weights are unchanged.
  - The game replaces the materials at runtime, to give each character its outfit colours.

The code in `src/mmo/hero.rs` depends on this clip order (`Clip`).

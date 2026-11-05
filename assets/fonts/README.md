# Embedded fonts

- Inter 4.1 (Regular, SemiBold), from https://github.com/rsms/inter/releases, SIL OFL 1.1 (Inter-LICENSE.txt)
- JetBrains Mono 2.304 (Regular), from https://github.com/JetBrains/JetBrainsMono/releases, SIL OFL 1.1 (JetBrainsMono-LICENSE.txt)

Both were subset with fontTools to remove the Unicode private-use range (U+E000-U+F8FF).
The app's Phosphor icons live in that range, and egui picks the first font in a family
that has a glyph, so Inter's own private-use glyphs would otherwise replace the icons.

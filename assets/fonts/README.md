# Embedded fonts

- Inter 4.1 (Regular, SemiBold), from https://github.com/rsms/inter/releases, SIL OFL 1.1 (Inter-LICENSE.txt)
- JetBrains Mono 2.304 (Regular), from https://github.com/JetBrains/JetBrainsMono/releases, SIL OFL 1.1 (JetBrainsMono-LICENSE.txt)

Both were subset with fontTools to remove the Unicode private-use range (U+E000-U+F8FF),
where the app's Phosphor icons live, so Inter's own private-use glyphs never stand in for them.

- Phosphor (Regular) icon font, from https://github.com/phosphor-icons/homepage via the
  egui-phosphor crate, MIT licence. Glyph code points are listed in `src/app/icons.rs`.

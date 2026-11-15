Fonts embedded in the app (see `src/ui/theme.rs`):
Inter 4.1 (Regular, Medium) and Source Serif 4.005 (Regular, Semibold), from their GitHub releases.
Licence: SIL Open Font License 1.1 (`Inter-LICENSE.txt`, `SourceSerif4-LICENSE.md`).

The Inter files have their Private Use Area mappings (U+E000-F8FF) removed so they don't shadow the Phosphor icon glyphs that share those codepoints (stripped with fontTools: delete those keys from every Unicode cmap subtable).

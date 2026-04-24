# RScribus - Project Progress

## Current Status
Functional prototype of a layout tool with WYSIWYG capabilities.

## Features Implemented
1.  **Document Model**: Multi-page support, items (Text, Image, Shape), and serialization (JSON).
2.  **Modern UI**: Built with `Libadwaita`, including HeaderBar, Sidebar, and Canvas.
3.  **Direct Interaction**:
    *   **Drag to Create**: Click and drag to create new text frames.
    *   **Selection**: Click on items to select them (blue highlight).
    *   **Movement**: Drag selected items to reposition them.
    *   **Resizing**: 8-handle resizing system for precision geometry control.
4.  **WYSIWYG Editing**:
    *   **Double Click**: Triggers in-place text editing.
    *   **Overlay Editor**: A `TextView` appears exactly over the object on the canvas.
    *   **Auto-save**: Saves changes when losing focus.
5.  **Contextual Info**: Right-click popover showing paragraphs, lines, words, and characters.

## Technical Details
*   **Engine**: Cairo for canvas rendering.
*   **Framework**: Relm4 (GTK4 + Libadwaita).
*   **Scaling**: 3 pixels per mm (allows real-world dimension mapping).

## Next Steps
*   Image support (ImageFrame).
*   Persistence (Save/Load dialogues).
*   Numerical property editing in the sidebar.
*   Delete/Duplicate shortcuts.

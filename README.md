# Photoframe

A Rust-based digital photo frame application designed to run on a Raspberry Pi.

## Status

This project is **alpha and under development**

## Features (Tier 1)

- Recursive/scoped directory scanning (configurable)
- Image type filtering (jpg/png/gif/webp/bmp/tiff)
- Circular buffer (infinite loop)
- Fixed per-image delay (configurable)
- Error handling and structured logging

## Event Flow

```mermaid
flowchart LR
  MAIN[Main] --> FILES[PhotoFiles]
  MAIN --> MAN[PhotoManager]
  MAIN --> LOAD[PhotoLoader]
  MAIN --> VIEW[PhotoViewer]

  FILES -->|add remove| MAN
  MAN -->|invalid exif| FILES
  MAN -->|load| LOAD
  LOAD -->|loaded| VIEW
  LOAD -->|invalid load| FILES
```

## License

This project is licensed under the **MIT License**.
See the [LICENSE](LICENSE) file for full text.

© 2025 Vincent Lucarelli

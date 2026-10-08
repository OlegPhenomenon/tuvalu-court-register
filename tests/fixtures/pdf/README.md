# Fictional DEMO PDF corpus

All visible text is fictional DEMO material. Generated on macOS with CUPS and
Quartz; each PDF is below 60 KiB. No personal or court documents are included.

- `cups-text.pdf`: CUPS text print, embedded TrueType font and Flate content.
- `cups-rtf-export.pdf`: RTF exported with textutil, then printed with CUPS.
- `quartz-truetype.pdf`: CoreText/Quartz text with embedded Arial TrueType font.
- `quartz-flate-image.pdf`: Quartz text and a Flate RGB image.

Regenerate from the repository root on macOS:

```sh
/usr/sbin/cupsfilter -m application/pdf tests/fixtures/pdf/demo.txt > tests/fixtures/pdf/cups-text.pdf
textutil -convert txt -output /tmp/tcr-demo-rtf.txt tests/fixtures/pdf/demo.rtf
/usr/sbin/cupsfilter -m application/pdf /tmp/tcr-demo-rtf.txt > tests/fixtures/pdf/cups-rtf-export.pdf
swift tests/fixtures/pdf/generate-quartz.swift "$PWD/tests/fixtures/pdf"
rm /tmp/tcr-demo-rtf.txt
```

Quartz/CUPS timestamps and font subset tags can change on regeneration. The test
checks clean storage verdicts and sizes, rather than byte-for-byte reproduction.

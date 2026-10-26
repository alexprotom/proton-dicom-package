# prad-rti-gen

Converts proton radiography WEPL maps - ROOT `TTree::Scan()` text output
(`Row | x.x | y.y | WEPL.WEPL`) - into DICOM RT Image files.

```text
prad-rti-gen RG_Output_*.txt -o dicom
prad-rti-gen --init-config        # commented prad-rti-gen.toml
```

`prad-rti-gen.toml` in this folder is the commented default configuration;
it is compiled into the executable as the built-in defaults.

Full documentation: [docs/prad-rti-gen.md](../../docs/prad-rti-gen.md).

# Introduction

Evelyn is an **Android ROM porting framework** written in Rust. A **port**
combines a ROM's *system side* with a device's own *vendor side*, applies
**patches** to them, and builds partition images, a `super.img`, or a
recovery-flashable ZIP.

Evelyn never unpacks the firmware it reads. EROFS and ext4 images, raw or
sparse, are read in place. Only the files a patch changes are written out.
Everything else streams straight from the source image into the new image.

## Device repositories

A **device repository** ports one ROM to one device. It holds:

- the device's description in `evelyn.toml`
- one file in `ports/` for each ROM version it supports
- the patches those ports apply

## Key features

- **In-place images.** Source images are scanned into trees whose files point
  back at the image. Untouched files are never copied to disk.
- **Treble-aware composition.** `vendor`, `odm`, `vendor_dlkm` and `odm_dlkm`
  always come from the stock firmware. `system`, `system_ext`, `product` and
  `system_dlkm` always come from the ROM. Anything else comes from the ROM only
  when the port asks for it.
- **Declarative patches.** A patch is a [RON](https://github.com/ron-rs/ron)
  recipe: named steps, each a list of operations applied in order. Operations
  can bind values read from the firmware, and apply only under a condition.
- **Deep edits.** Recipes can do all of these:
  - edit properties, text, bytes and XML
  - add SELinux policy, or import it from a compiled policy
  - edit APKs and JARs: their files, their values resources and their smali
    code, and re-sign them
- **Shared operations.** Kits define operations out of the built-in ones.
  Evelyn ships kits for ROM families, and a device repository can add its own.
- **OTA packages as input.** Evelyn reads a full OTA package, local or at an
  `http(s)://` URL, and extracts only the partitions a build reads. For a remote
  package, only those partitions are downloaded.
- **Three outputs.** Evelyn can write partition images, a `super` image, or a
  recovery-flashable ZIP.

## How it fits together

<figure class="diagram">
<svg viewBox="0 0 640 530" width="640" role="img" aria-label="The ROM supplies the system side and the stock firmware the vendor side. The port is picked, its patches and the built-in recipes are applied, and the result is written as partition images, a super image or a recovery ZIP.">
<defs><marker id="flow-head" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="head" d="M0,0 L10,5 L0,10 z"/></marker></defs>
<rect class="box" x="40" y="10" width="260" height="54" rx="8"/>
<text class="title" x="170" y="33">ROM</text>
<text class="detail" x="170" y="53">firmware/rom</text>
<rect class="box" x="340" y="10" width="260" height="54" rx="8"/>
<text class="title" x="470" y="33">Stock firmware</text>
<text class="detail" x="470" y="53">firmware/stock</text>
<path class="line" d="M170,64 V92" marker-end="url(#flow-head)"/>
<path class="line" d="M470,64 V92" marker-end="url(#flow-head)"/>
<rect class="box" x="40" y="94" width="260" height="74" rx="8"/>
<text class="title" x="170" y="117">System side</text>
<text class="detail" x="170" y="137">system · system_ext · product</text>
<text class="detail" x="170" y="155">system_dlkm · what the port lists</text>
<rect class="box" x="340" y="94" width="260" height="74" rx="8"/>
<text class="title" x="470" y="117">Vendor side</text>
<text class="detail" x="470" y="137">vendor · odm</text>
<text class="detail" x="470" y="155">vendor_dlkm · odm_dlkm</text>
<path class="line" d="M170,168 V186 H470 V168"/>
<path class="line" d="M320,186 V210" marker-end="url(#flow-head)"/>
<rect class="box" x="160" y="212" width="320" height="54" rx="8"/>
<text class="title" x="320" y="235">Pick the port</text>
<text class="detail" x="320" y="255">ports/*.toml</text>
<path class="line" d="M320,266 V290" marker-end="url(#flow-head)"/>
<rect class="box accent" x="160" y="292" width="320" height="54" rx="8"/>
<text class="title" x="320" y="315">Apply the port's patches, in order</text>
<text class="detail" x="320" y="335">patches/*/patch.ron</text>
<path class="line" d="M320,346 V370" marker-end="url(#flow-head)"/>
<rect class="box" x="160" y="372" width="320" height="54" rx="8"/>
<text class="title" x="320" y="395">Apply the built-in recipes</text>
<text class="detail" x="320" y="415">VNDK APEX, --debug</text>
<path class="line" d="M320,426 V440 H115 V462" marker-end="url(#flow-head)"/>
<path class="line" d="M320,440 V462" marker-end="url(#flow-head)"/>
<path class="line" d="M320,440 H525 V462" marker-end="url(#flow-head)"/>
<rect class="box" x="30" y="464" width="170" height="54" rx="8"/>
<text class="title" x="115" y="487">Partition images</text>
<text class="detail" x="115" y="507">one per partition</text>
<rect class="box" x="235" y="464" width="170" height="54" rx="8"/>
<text class="title" x="320" y="487">Super image</text>
<text class="detail" x="320" y="507">super.img</text>
<rect class="box" x="440" y="464" width="170" height="54" rx="8"/>
<text class="title" x="525" y="487">Recovery ZIP</text>
<text class="detail" x="525" y="507">flashable</text>
</svg>
<figcaption>Figure 1: How a build is put together. The output is written to <code>out/&lt;port&gt;/</code>.</figcaption>
</figure>

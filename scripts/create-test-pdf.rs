//! Creates a deterministic PDF used for integration and visual testing.
//!
//! This intentionally uses only the Rust standard library. The fixture covers
//! different page sizes, inherited rotation, a CropBox, RGB primaries, a
//! nested outline, and a ToUnicode map so PDFium's document, raster, and text
//! paths are exercised together.

use std::env;
use std::fs;
use std::path::PathBuf;

fn stream(dictionary: &str, bytes: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} /Length {} >>\nstream\n", bytes.len()).into_bytes();
    object.extend_from_slice(bytes);
    object.extend_from_slice(b"\nendstream");
    object
}

fn main() {
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/interaction.pdf"));

    let page_one = br#"q
1 0 0 rg 48 650 150 70 re f
0 1 0 rg 231 650 150 70 re f
0 0 1 rg 414 650 150 70 re f
0 0 0 RG 2 w 48 650 516 70 re S
Q
BT /F1 28 Tf 54 590 Td (GPUI PDF Reader integration fixture) Tj ET
BT /F1 16 Tf 54 550 Td (Page 1 - portrait - RGB blocks above) Tj ET
BT /F1 13 Tf 54 510 Td (Select this sentence, copy it, and compare exactly.) Tj ET
BT /F1 13 Tf 54 480 Td (Horizontal scrolling appears after zooming in.) Tj ET
BT /F2 12 Tf 3 Tr 54 440 Td <004700500055004900200050004400460020005200650061006400650072002000A9002003A900204F60597D2014> Tj ET
"#;

    let page_two = br#"q
0.93 0.95 1 rg 0 0 792 612 re f
0.85 0.18 0.12 rg 40 40 180 60 re f
0.10 0.55 0.24 rg 572 512 180 60 re f
Q
BT /F1 26 Tf 70 440 Td (Page 2 - Rotate 90) Tj ET
BT /F1 15 Tf 70 400 Td (The red marker begins at the unrotated lower left.) Tj ET
BT /F1 15 Tf 70 370 Td (The green marker ends at the unrotated upper right.) Tj ET
BT /F1 13 Tf 70 310 Td (Rotated text hit boxes must still align with these glyphs.) Tj ET
"#;

    let page_three = br#"q
0.96 0.92 0.82 rg 0 0 720 432 re f
0.18 0.35 0.72 rg 36 36 648 360 re S
0.18 0.35 0.72 rg 40 330 640 44 re f
Q
BT /F1 24 Tf 58 280 Td (Page 3 - wide CropBox) Tj ET
BT /F1 14 Tf 58 240 Td (Only the region inside the blue outline should be visible.) Tj ET
BT /F1 14 Tf 58 205 Td (Cross-page selection should preserve page order.) Tj ET
"#;

    let cmap = br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /GPUIPDFReaderUnicode def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfrange
<0000> <00FF> <0000>
endbfrange
4 beginbfchar
<03A9> <03A9>
<4F60> <4F60>
<597D> <597D>
<2014> <2014>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end
"#;

    // Object numbers are intentionally stable so the fixture is reproducible.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R /Outlines 14 0 R /PageMode /UseOutlines >>".to_vec(),
        b"<< /Type /Pages /Count 3 /Kids [3 0 R 5 0 R 7 0 R] /Resources << /Font << /F1 9 0 R /F2 10 0 R >> >> >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        stream("", page_one),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Rotate 90 /Contents 6 0 R >>".to_vec(),
        stream("", page_two),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 720 432] /CropBox [36 36 684 396] /Contents 8 0 R >>".to_vec(),
        stream("", page_three),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Helvetica /Encoding /Identity-H /DescendantFonts [11 0 R] /ToUnicode 12 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 13 0 R /DW 600 /CIDToGIDMap /Identity >>".to_vec(),
        stream("", cmap),
        b"<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /FontBBox [-166 -225 1000 931] /ItalicAngle 0 /Ascent 718 /Descent -207 /CapHeight 718 /StemV 80 >>".to_vec(),
        b"<< /Type /Outlines /First 15 0 R /Last 18 0 R /Count 4 >>".to_vec(),
        b"<< /Title (Getting Started) /Parent 14 0 R /Next 17 0 R /First 16 0 R /Last 16 0 R /Count 1 /Dest [3 0 R /FitH 760] >>".to_vec(),
        b"<< /Title (Selecting text) /Parent 15 0 R /Dest [3 0 R /XYZ 54 530 null] >>".to_vec(),
        b"<< /Title (Page 2 - Rotate 90) /Parent 14 0 R /Prev 15 0 R /Next 18 0 R /A << /S /GoTo /D [5 0 R /Fit] >> >>".to_vec(),
        b"<< /Title (Wide documents) /Parent 14 0 R /Prev 17 0 R /Dest [7 0 R /FitH 396] >>".to_vec(),
    ];

    let mut pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("could not create fixture directory");
    }
    fs::write(&output, pdf).expect("could not write fixture PDF");
    println!("wrote {}", output.display());
}

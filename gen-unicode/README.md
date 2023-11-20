# gen-unicode

This crate generates unicode tables and code specific for regress.

## Usage

1. Download the needed unicode source files:

    ```sh
    curl -L http://ftp.unicode.org/Public/UNIDATA/CaseFolding.txt -o CaseFolding.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/extracted/DerivedBinaryProperties.txt -o DerivedBinaryProperties.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/DerivedCoreProperties.txt -o DerivedCoreProperties.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/extracted/DerivedGeneralCategory.txt -o DerivedGeneralCategory.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/DerivedNormalizationProps.txt -o DerivedNormalizationProps.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/emoji/emoji-data.txt -o emoji-data.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/PropList.txt -o PropList.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/Scripts.txt -o Scripts.txt
    mkdir /tmp/ucd-15.0.0
    cd /tmp/ucd-15.0.0
    curl -LO https://www.unicode.org/Public/zipped/15.0.0/UCD.zip
    unzip UCD.zip
    ```

2. Run this crate and redirect the output in the specific rs file in the regress crate:

    ```sh
    cargo run
    ```

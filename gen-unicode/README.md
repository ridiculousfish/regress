# gen-unicode

This crate generates unicode tables and code specific for regress.

## Usage

1. Download the needed unicode source files:

    ```sh
    curl -L http://ftp.unicode.org/Public/UNIDATA/CaseFolding.txt -o CaseFolding.txt
    curl -L http://ftp.unicode.org/Public/UNIDATA/DerivedCoreProperties.txt -o DerivedCoreProperties.txt
    ```

2. Run this crate and redirect the output in the specific rs file in the regress crate:

    ```sh
    cargo run > ../src/unicode.rs
    ```

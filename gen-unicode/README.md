# gen-unicode

This crate generates unicode tables and code specific for regress.

## Usage

1. Download the needed unicode source files:

    ```sh
    mkdir /tmp/ucd-15.1.0
    cd /tmp/ucd-15.1.0
    curl -LO https://www.unicode.org/Public/zipped/15.1.0/UCD.zip
    unzip UCD.zip
    curl -LO https://www.unicode.org/Public/emoji/15.1/emoji-sequences.txt
    curl -LO https://www.unicode.org/Public/emoji/15.1/emoji-zwj-sequences.txt
    ```

2. Run this crate and redirect the output in the specific rs file in the regress crate:

    ```sh
    cargo run
    ```

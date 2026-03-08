# gen-unicode

This crate generates unicode tables and code specific for regress.

## Usage

1. Download the needed unicode source files:

    ```sh
    mkdir /tmp/ucd-17.0.0
    cd /tmp/ucd-17.0.0
    curl -LO https://www.unicode.org/Public/zipped/latest/UCD.zip
    unzip UCD.zip
    curl -LO https://www.unicode.org/Public/emoji/latest/emoji-sequences.txt
    curl -LO https://www.unicode.org/Public/emoji/latest/emoji-zwj-sequences.txt
    ```

2. Run this crate and redirect the output in the specific rs file in the regress crate:

    ```sh
    cargo run
    ```

# Bundled test model: stories260K

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `stories260K.bin` | 1,056,540 | `b0a507e7ad0f626624f17112325e66691f9076d622e1d3274d103d00299f2696` |
| `tok512.bin` | 6,227 | `037cb335abb25d1fa9e8ecae30ed2a3a8ace9302862ebcdc05d51a6bbb10c312` |
| `stories260K-greedy-200.txt` | 464 | reference output, see below |

`stories260K.bin` and `tok512.bin` are Andrej Karpathy's 260K-parameter
TinyStories Llama-architecture model and its 512-token tokenizer in llama2.c
format, published at `huggingface.co/karpathy/tinyllamas` (`stories260K/`)
under the MIT licence of [llama2.c](https://github.com/karpathy/llama2.c)
(copied as [LICENSE-llama2.c](LICENSE-llama2.c)). Configuration: dim 64,
hidden 172, 5 layers, 8 query heads, 4 KV heads (grouped-query attention),
vocabulary 512, context 512, classifier shared with the token embedding.

Hugging Face is not reachable from the cloud host this was prepared on, so the
files were taken from two independent public GitHub mirrors that carry
byte-identical copies (git blob `cfebf5a594edee35c18c88b12ef45de3b6d9aaa9` and
`be175e5ee3902928bcbaa538d4a62e462fe56414`): `irhamuba/qiubit-research`
(`weights/`, commit `3527c5a9`) and `wbrown/tinyoai` (`assets/`, commit
`90338dac`). The checkpoint's size matches its own header exactly.

`stories260K-greedy-200.txt` is the `expected_stdout` literal from llama2.c's
`test_all.py` at commit `350e04fe35433e6d2941dce5a1f53308f87058eb`: the text
that upstream checks both `run.c` (`./run stories260K.bin -z tok512.bin -t 0.0
-n 200`) and the PyTorch `model.py` produce for 200 greedy steps from BOS. The
file has no trailing newline (the test strips the one `run.c` prints). That
the Vesper implementation reproduces this text byte for byte is the evidence
that the files above are the upstream ones and that the forward pass is right.

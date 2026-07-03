Perl の subroutine reference を引数として受け取り、そのコードをOP Tree レベルで解析し、

- 引数, 戻り値（又は例外として投げるもの）の仕様
- 内部のロジックの、真理値表
- （可能な範囲で）変数の型

を調べるための機能を Rust と Perl の組み合わせで作りたいと考えています。
（機能は Perl のコードから呼び出せるライブラリにする必要があります。
これは eval の返した anonymous sub に対する解析を行いたいからです）

Rust から Perl にアクセスするためのライブラリとしては私の作った [libperl-rs](https://github.com/hkoba/libperl-rs) を使います。
もし libperl-rs （や関連クレート libperl-macrogen）に機能不足がある場合は
そちらを改良することもプロジェクトのスコープに含まれます。

OP Tree をどう解釈するかについては B::Deparse モジュールの内部の挙動を参考にしたいと
考えています。

参考のため、本件で gemini に質問した時の対話も[こちら](gemini-advice.md)に置きました。

以上の要望を実現するための計画を立てて下さい。

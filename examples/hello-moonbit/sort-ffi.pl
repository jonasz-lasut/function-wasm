#!/usr/bin/env perl
# wit-bindgen's MoonBit generator (0.62.0) writes the `extern "wasm"` FFI
# helpers of each ffi*.mbt in hash order, which differs from one run to the
# next, so the checked-in bindings would never match a regeneration and the
# render job's drift check could not tell a real change from noise. This
# orders them: every other block of the file keeps its place, the helper
# blocks follow it sorted. MoonBit does not care about declaration order.
# make gen-bindings runs it over every ffi*.mbt the generator wrote.
use strict;
use warnings;

local $/;
for my $path (@ARGV) {
    open my $in, '<', $path or die "$path: $!";
    my $text = <$in>;
    close $in;
    my (@rest, @helpers);
    for my $block (split /\n\n/, $text) {
        # A helper: the doc marker, any attribute lines (#owned(...)), the
        # extern.
        if ($block =~ /^\/\/\/\|\n(?:#\w+\([^)]*\)\n)*extern "wasm" fn mbt_ffi_/) {
            push @helpers, $block;
        } else {
            push @rest, $block;
        }
    }
    my $out = join("\n\n", @rest, sort @helpers) . "\n";
    open my $outfh, '>', $path or die "$path: $!";
    print $outfh $out;
    close $outfh;
}

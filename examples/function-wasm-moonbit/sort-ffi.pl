#!/usr/bin/env perl
# wit-bindgen's MoonBit generator (0.62.0) writes the `extern "wasm"` FFI
# helpers of each ffi*.mbt, and the #doc(hidden) wrappers of the exports the
# component links (wasmExportRun, wasmExportRunPostReturn), in hash order,
# which differs from one run to the next, so the checked-in bindings would
# never match a regeneration and the render job's drift check could not tell
# a real change from noise. This orders them: every other block of the file
# keeps its place, then the wrapper functions sorted, then the helper blocks
# sorted. MoonBit does not care about declaration order. make gen-bindings
# runs it over every ffi*.mbt the generator wrote.
use strict;
use warnings;

local $/;
for my $path (@ARGV) {
    open my $in, '<', $path or die "$path: $!";
    my $text = <$in>;
    close $in;
    my @blocks = split /\n\n/, $text;
    my (@rest, @wrappers, @helpers);
    while (defined(my $block = shift @blocks)) {
        # A helper: the doc marker, any attribute lines (#owned(...)), the
        # extern.
        if ($block =~ /^\/\/\/\|\n(?:#\w+\([^)]*\)\n)*extern "wasm" fn mbt_ffi_/) {
            push @helpers, $block;
        }
        # An export wrapper: the hidden-doc marker over the pub fn the link
        # section names. Its body may hold blank lines (the world package's
        # does), so the function runs on to the block that closes it.
        elsif ($block =~ /^#doc\(hidden\)\npub fn wasmExport/) {
            $block .= "\n\n" . shift @blocks while @blocks && $block !~ /^\}\s*\z/m;
            push @wrappers, $block;
        }
        else {
            push @rest, $block;
        }
    }
    my $out = join("\n\n", @rest, sort(@wrappers), sort(@helpers)) . "\n";
    open my $outfh, '>', $path or die "$path: $!";
    print $outfh $out;
    close $outfh;
}

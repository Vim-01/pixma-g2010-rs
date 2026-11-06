
use builtin;
use str;

set edit:completion:arg-completer[pixma-cli] = {|@words|
    fn spaces {|n|
        builtin:repeat $n ' ' | str:join ''
    }
    fn cand {|text desc|
        edit:complex-candidate $text &display=$text' '(spaces (- 14 (wcswidth $text)))$desc
    }
    var command = 'pixma-cli'
    for word $words[1..-1] {
        if (str:has-prefix $word '-') {
            break
        }
        set command = $command';'$word
    }
    var completions = [
        &'pixma-cli'= {
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
            cand -V 'Print version'
            cand --version 'Print version'
            cand status 'Query real-time hardware status, ink levels, and diagnostic error codes'
            cand clean 'Execute print head cleaning routine'
            cand roller-clean 'Clean mechanical paper feed rollers or platen bottom plate ribs'
            cand nozzle-check 'Print diagnostic nozzle check calibration grid'
            cand align 'Print head alignment routines and adjustment offsets'
            cand scan 'Acquire image from the flatbed scanner platen'
            cand print 'Print an image, PWG Raster document, or internal test page'
            cand completions 'Generate shell autocompletion scripts (bash, zsh, fish, powershell, elvish)'
            cand help 'Print this message or the help of the given subcommand(s)'
        }
        &'pixma-cli;status'= {
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;clean'= {
            cand --kind 'Cleaning intensity'
            cand --target 'Cleaning target ink group'
            cand --confirm 'Mandatory confirmation token for Ink Flush (''YES-DRAIN-INK'')'
            cand --socket 'Path to pixmad Unix domain socket'
            cand --override-cooldown-dangerously 'Bypass mandatory cooldown timers'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;roller-clean'= {
            cand --target 'Mechanical target'
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;nozzle-check'= {
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;align'= {
            cand --action 'Alignment action'
            cand --values '14 comma-separated integer offsets (-5..=5) for columns A..N'
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;scan'= {
            cand --dpi 'Scanning resolution (75, 100, 150, 200, 300, 400, 600, 1200)'
            cand --mode 'Color mode'
            cand --output 'Output destination file path (.pdf, .png, .jpg, .tiff)'
            cand --width 'Scan region width in pixels'
            cand --height 'Scan region height in pixels'
            cand --gamma 'Midtone gamma adjustment'
            cand --socket 'Path to pixmad Unix domain socket'
            cand --auto-crop 'Enable automatic multi-photo crop'
            cand --deskew 'Enable automatic document deskew'
            cand --descreen 'Enable 2D-FFT halftone descreening'
            cand --unsharp 'Enable Gaussian unsharp masking edge sharpening'
            cand --remove-dust 'Enable dust and scratch removal'
            cand --gutter-shadow 'Enable gutter shadow compensation'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;print'= {
            cand --input 'Input image or PWG Raster file path'
            cand --media 'Paper media type'
            cand --paper 'Paper size'
            cand --quality 'Print quality level (1 = Finest, 5 = Draft)'
            cand --borderless 'Borderless overscan mode'
            cand --layout 'Page layout mode'
            cand --cyan 'Cyan color balance adjustment (-50..50)'
            cand --magenta 'Magenta color balance adjustment (-50..50)'
            cand --yellow 'Yellow color balance adjustment (-50..50)'
            cand --brightness 'Brightness adjustment (-50..50)'
            cand --contrast 'Contrast adjustment (-50..50)'
            cand --drying 'Ink drying wait time'
            cand --socket 'Path to pixmad Unix domain socket'
            cand --test-page 'Print diagnostic test page'
            cand --grayscale 'Convert printout to grayscale'
            cand --wide-gap 'Prevent paper abrasion (wide platen gap)'
            cand --quiet 'Enable acoustic quiet mode'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;completions'= {
            cand --socket 'Path to pixmad Unix domain socket'
            cand --mock 'Use in-memory virtual G2010 hardware emulator'
            cand --json 'Emit output in machine-readable JSON format'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'pixma-cli;help'= {
            cand status 'Query real-time hardware status, ink levels, and diagnostic error codes'
            cand clean 'Execute print head cleaning routine'
            cand roller-clean 'Clean mechanical paper feed rollers or platen bottom plate ribs'
            cand nozzle-check 'Print diagnostic nozzle check calibration grid'
            cand align 'Print head alignment routines and adjustment offsets'
            cand scan 'Acquire image from the flatbed scanner platen'
            cand print 'Print an image, PWG Raster document, or internal test page'
            cand completions 'Generate shell autocompletion scripts (bash, zsh, fish, powershell, elvish)'
            cand help 'Print this message or the help of the given subcommand(s)'
        }
        &'pixma-cli;help;status'= {
        }
        &'pixma-cli;help;clean'= {
        }
        &'pixma-cli;help;roller-clean'= {
        }
        &'pixma-cli;help;nozzle-check'= {
        }
        &'pixma-cli;help;align'= {
        }
        &'pixma-cli;help;scan'= {
        }
        &'pixma-cli;help;print'= {
        }
        &'pixma-cli;help;completions'= {
        }
        &'pixma-cli;help;help'= {
        }
    ]
    $completions[$command]
}

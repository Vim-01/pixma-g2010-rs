# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_pixma_cli_global_optspecs
    string join \n socket= mock json h/help V/version
end

function __fish_pixma_cli_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_pixma_cli_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_pixma_cli_using_subcommand
    set -l cmd (__fish_pixma_cli_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -s V -l version -d 'Print version'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "status" -d 'Query real-time hardware status, ink levels, and diagnostic error codes'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "clean" -d 'Execute print head cleaning routine'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "roller-clean" -d 'Clean mechanical paper feed rollers or platen bottom plate ribs'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "nozzle-check" -d 'Print diagnostic nozzle check calibration grid'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "align" -d 'Print head alignment routines and adjustment offsets'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "scan" -d 'Acquire image from the flatbed scanner platen'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "print" -d 'Print an image, PWG Raster document, or internal test page'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "completions" -d 'Generate shell autocompletion scripts (bash, zsh, fish, powershell, elvish)'
complete -c pixma-cli -n "__fish_pixma_cli_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand status" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand status" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand status" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand status" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l kind -d 'Cleaning intensity' -r -f -a "regular\t''
deep\t''
flush\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l target -d 'Cleaning target ink group' -r -f -a "all\t''
black\t''
color\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l confirm -d 'Mandatory confirmation token for Ink Flush (\'YES-DRAIN-INK\')' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l override-cooldown-dangerously -d 'Bypass mandatory cooldown timers'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand clean" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand roller-clean" -l target -d 'Mechanical target' -r -f -a "roller\t''
platen\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand roller-clean" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand roller-clean" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand roller-clean" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand roller-clean" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand nozzle-check" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand nozzle-check" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand nozzle-check" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand nozzle-check" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -l action -d 'Alignment action' -r -f -a "print-manual\t''
half-auto\t''
check\t''
apply\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -l values -d '14 comma-separated integer offsets (-5..=5) for columns A..N' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand align" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l dpi -d 'Scanning resolution (75, 100, 150, 200, 300, 400, 600, 1200)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l mode -d 'Color mode' -r -f -a "color\t''
gray\t''
lineart\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l output -d 'Output destination file path (.pdf, .png, .jpg, .tiff)' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l width -d 'Scan region width in pixels' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l height -d 'Scan region height in pixels' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l gamma -d 'Midtone gamma adjustment' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l auto-crop -d 'Enable automatic multi-photo crop'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l deskew -d 'Enable automatic document deskew'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l descreen -d 'Enable 2D-FFT halftone descreening'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l unsharp -d 'Enable Gaussian unsharp masking edge sharpening'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l remove-dust -d 'Enable dust and scratch removal'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l gutter-shadow -d 'Enable gutter shadow compensation'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand scan" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l input -d 'Input image or PWG Raster file path' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l media -d 'Paper media type' -r -f -a "plain\t''
glossygold\t''
proplat\t''
luster\t''
semigloss\t''
glossy\t''
matte\t''
highres\t''
envelope\t''
hagaki\t''
cardstock\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l paper -d 'Paper size' -r -f -a "a4\t''
a5\t''
b5\t''
letter\t''
legal\t''
4x6\t''
5x7\t''
8x10\t''
l\t''
square5x5\t''
businesscard\t''
env10\t''
envdl\t''
hagaki\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l quality -d 'Print quality level (1 = Finest, 5 = Draft)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l borderless -d 'Borderless overscan mode' -r -f -a "off\t''
0\t''
1\t''
2\t''
3\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l layout -d 'Page layout mode' -r -f -a "normal\t''
fit\t''
scaled\t''
nup2\t''
nup4\t''
nup6\t''
nup9\t''
nup16\t''
poster2x2\t''
poster3x3\t''
booklet\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l cyan -d 'Cyan color balance adjustment (-50..50)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l magenta -d 'Magenta color balance adjustment (-50..50)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l yellow -d 'Yellow color balance adjustment (-50..50)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l brightness -d 'Brightness adjustment (-50..50)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l contrast -d 'Contrast adjustment (-50..50)' -r
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l drying -d 'Ink drying wait time' -r -f -a "short\t''
normal\t''
long\t''
extralong\t''"
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l test-page -d 'Print diagnostic test page'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l grayscale -d 'Convert printout to grayscale'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l wide-gap -d 'Prevent paper abrasion (wide platen gap)'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l quiet -d 'Enable acoustic quiet mode'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand print" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand completions" -l socket -d 'Path to pixmad Unix domain socket' -r -F
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand completions" -l mock -d 'Use in-memory virtual G2010 hardware emulator'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand completions" -l json -d 'Emit output in machine-readable JSON format'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand completions" -s h -l help -d 'Print help'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "status" -d 'Query real-time hardware status, ink levels, and diagnostic error codes'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "clean" -d 'Execute print head cleaning routine'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "roller-clean" -d 'Clean mechanical paper feed rollers or platen bottom plate ribs'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "nozzle-check" -d 'Print diagnostic nozzle check calibration grid'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "align" -d 'Print head alignment routines and adjustment offsets'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "scan" -d 'Acquire image from the flatbed scanner platen'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "print" -d 'Print an image, PWG Raster document, or internal test page'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "completions" -d 'Generate shell autocompletion scripts (bash, zsh, fish, powershell, elvish)'
complete -c pixma-cli -n "__fish_pixma_cli_using_subcommand help; and not __fish_seen_subcommand_from status clean roller-clean nozzle-check align scan print completions help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'

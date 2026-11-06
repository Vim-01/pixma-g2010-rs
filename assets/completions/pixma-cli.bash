_pixma__cli() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
    prev="$3"
    cmd=""
    opts=""

    for i in "${COMP_WORDS[@]:0:COMP_CWORD}"
    do
        case "${cmd},${i}" in
            ",$1")
                cmd="pixma__cli"
                ;;
            pixma__cli,align)
                cmd="pixma__cli__subcmd__align"
                ;;
            pixma__cli,clean)
                cmd="pixma__cli__subcmd__clean"
                ;;
            pixma__cli,completions)
                cmd="pixma__cli__subcmd__completions"
                ;;
            pixma__cli,help)
                cmd="pixma__cli__subcmd__help"
                ;;
            pixma__cli,nozzle-check)
                cmd="pixma__cli__subcmd__nozzle__subcmd__check"
                ;;
            pixma__cli,print)
                cmd="pixma__cli__subcmd__print"
                ;;
            pixma__cli,roller-clean)
                cmd="pixma__cli__subcmd__roller__subcmd__clean"
                ;;
            pixma__cli,scan)
                cmd="pixma__cli__subcmd__scan"
                ;;
            pixma__cli,status)
                cmd="pixma__cli__subcmd__status"
                ;;
            pixma__cli__subcmd__help,align)
                cmd="pixma__cli__subcmd__help__subcmd__align"
                ;;
            pixma__cli__subcmd__help,clean)
                cmd="pixma__cli__subcmd__help__subcmd__clean"
                ;;
            pixma__cli__subcmd__help,completions)
                cmd="pixma__cli__subcmd__help__subcmd__completions"
                ;;
            pixma__cli__subcmd__help,help)
                cmd="pixma__cli__subcmd__help__subcmd__help"
                ;;
            pixma__cli__subcmd__help,nozzle-check)
                cmd="pixma__cli__subcmd__help__subcmd__nozzle__subcmd__check"
                ;;
            pixma__cli__subcmd__help,print)
                cmd="pixma__cli__subcmd__help__subcmd__print"
                ;;
            pixma__cli__subcmd__help,roller-clean)
                cmd="pixma__cli__subcmd__help__subcmd__roller__subcmd__clean"
                ;;
            pixma__cli__subcmd__help,scan)
                cmd="pixma__cli__subcmd__help__subcmd__scan"
                ;;
            pixma__cli__subcmd__help,status)
                cmd="pixma__cli__subcmd__help__subcmd__status"
                ;;
            *)
                ;;
        esac
    done

    case "${cmd}" in
        pixma__cli)
            opts="-h -V --socket --mock --json --help --version status clean roller-clean nozzle-check align scan print completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__align)
            opts="-h --action --values --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --action)
                    COMPREPLY=($(compgen -W "print-manual half-auto check apply" -- "${cur}"))
                    return 0
                    ;;
                --values)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__clean)
            opts="-h --kind --target --confirm --override-cooldown-dangerously --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --kind)
                    COMPREPLY=($(compgen -W "regular deep flush" -- "${cur}"))
                    return 0
                    ;;
                --target)
                    COMPREPLY=($(compgen -W "all black color" -- "${cur}"))
                    return 0
                    ;;
                --confirm)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__completions)
            opts="-h --socket --mock --json --help bash elvish fish powershell zsh"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help)
            opts="status clean roller-clean nozzle-check align scan print completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__align)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__clean)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__completions)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__nozzle__subcmd__check)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__print)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__roller__subcmd__clean)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__scan)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__help__subcmd__status)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__nozzle__subcmd__check)
            opts="-h --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__print)
            opts="-h --input --test-page --media --paper --quality --borderless --layout --grayscale --cyan --magenta --yellow --brightness --contrast --wide-gap --drying --quiet --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --input)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --media)
                    COMPREPLY=($(compgen -W "plain glossygold proplat luster semigloss glossy matte highres envelope hagaki cardstock" -- "${cur}"))
                    return 0
                    ;;
                --paper)
                    COMPREPLY=($(compgen -W "a4 a5 b5 letter legal 4x6 5x7 8x10 l square5x5 businesscard env10 envdl hagaki" -- "${cur}"))
                    return 0
                    ;;
                --quality)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --borderless)
                    COMPREPLY=($(compgen -W "off 0 1 2 3" -- "${cur}"))
                    return 0
                    ;;
                --layout)
                    COMPREPLY=($(compgen -W "normal fit scaled nup2 nup4 nup6 nup9 nup16 poster2x2 poster3x3 booklet" -- "${cur}"))
                    return 0
                    ;;
                --cyan)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --magenta)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --yellow)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --brightness)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --contrast)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --drying)
                    COMPREPLY=($(compgen -W "short normal long extralong" -- "${cur}"))
                    return 0
                    ;;
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__roller__subcmd__clean)
            opts="-h --target --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --target)
                    COMPREPLY=($(compgen -W "roller platen" -- "${cur}"))
                    return 0
                    ;;
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__scan)
            opts="-h --dpi --mode --output --width --height --auto-crop --deskew --descreen --unsharp --remove-dust --gutter-shadow --gamma --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --dpi)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --mode)
                    COMPREPLY=($(compgen -W "color gray lineart" -- "${cur}"))
                    return 0
                    ;;
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --width)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --height)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --gamma)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pixma__subcmd__cli__subcmd__status)
            opts="-h --socket --mock --json --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --socket)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _pixma__cli -o nosort -o bashdefault -o default pixma-cli
else
    complete -F _pixma__cli -o bashdefault -o default pixma-cli
fi

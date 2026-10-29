# Session-local defaults; never writes user configuration or downloads plugins.
[[ -o interactive && $TERM != dumb && ${+NO_COLOR} == 0 && ${TERMIOR_SHELL_COLORS:-1} != 0 ]] || return 0

# Respect prompts installed by user configuration. Prompt hooks run afterwards
# and remain free to set their own prompt; unrelated hooks (e.g. autosuggestions)
# must not disable the default colors.
if [[ $PS1 == "$__termior_initial_prompt" ]] &&
   (( ! ${+functions[precmd]} )); then
    PS1='%F{green}%n%f@%F{cyan}%m%f %F{blue}%1~%f %(?.%F{green}.%F{red})%#%f '
fi

# Use automatic colors so redirects/pipelines remain plain. Existing aliases
# and shell functions win. Git settings apply to this invocation only.
export CLICOLOR=${CLICOLOR:-1}
export LSCOLORS=${LSCOLORS:-ExFxCxDxBxegedabagacad}
if (( ! ${+aliases[ls]} && ! ${+functions[ls]} )); then
    case $OSTYPE in
        darwin*|freebsd*) alias ls='ls -G' ;;
        linux*) alias ls='ls --color=auto' ;;
    esac
fi
if (( ! ${+aliases[grep]} && ! ${+functions[grep]} )); then
    case $OSTYPE in
        darwin*|freebsd*|linux*) alias grep='grep --color=auto' ;;
    esac
fi
if (( ! ${+aliases[git]} && ! ${+functions[git]} )); then
    alias git='git -c color.ui=auto'
fi

# zsh 5.9 introduced memo tags for region_highlight. Older versions retain
# the prompt/output defaults without risking other widgets' highlight regions.
autoload -Uz is-at-least add-zle-hook-widget
is-at-least 5.9 || return 0
(( ${+functions[_zsh_highlight]} || ${+functions[_fast_highlight]} )) && return 0

# A deliberately small lexical highlighter, not a shell parser. (z) tokenizes
# without evaluating substitutions; (Q) removes quotes without executing them.
# Only our own regions are replaced; key bindings and other widgets survive.
__termior_highlight() {
    emulate -L zsh
    setopt extendedglob
    region_highlight=("${(@)region_highlight:#*memo=termior}")
    (( ${#BUFFER} <= 8192 )) || return 0
    local rest=$BUFFER token prefix word style
    local -a tokens
    integer offset=0 start=0 command_position=1 redirect_target=0
    tokens=("${(@z)BUFFER}")
    for token in "${tokens[@]}"; do
        prefix=${rest%%"$token"*}
        (( offset += ${#prefix} ))
        start=$offset
        (( offset += ${#token} ))
        rest=${rest[$(( ${#prefix} + ${#token} + 1 )),-1]}
        word=${(Q)token}
        style=''
        case $token in
            ';'|'|'|'||'|'&&'|'&'|'('|')'|$'\n')
                style='fg=magenta'
                command_position=1
                redirect_target=0 ;;
            '<'|'>'|'>>'|'<<'|'<<<'|'>&'|'&>')
                style='fg=magenta'
                redirect_target=1 ;;
            *)
                if (( redirect_target )); then
                    redirect_target=0
                elif (( command_position )); then
                    if [[ $token == [A-Za-z_][A-Za-z0-9_]#=* ]]; then
                        style='fg=magenta'
                    else
                        if builtin whence -w -- "$word" >/dev/null 2>&1; then
                            style='fg=green'
                        fi
                        command_position=0
                    fi
                elif [[ $token == -* ]]; then
                    style='fg=cyan'
                fi
                case $token in
                    \"*|\'*) style='fg=yellow' ;;
                    \$*) style='fg=magenta' ;;
                esac ;;
        esac
        [[ -n $style ]] && region_highlight+=("$start $offset $style memo=termior")
    done
    return 0
}
add-zle-hook-widget line-pre-redraw __termior_highlight

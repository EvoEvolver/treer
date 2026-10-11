# Embedded in install.sh after the CLI and Controller have been installed.
# A piped installer cannot change its parent shell; persist PATH for new shells.
configure_shell_path() {
    quoted_install_dir=$(printf "'%s'" "$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")")
    path_line="case \":\${PATH-}:\" in *:$quoted_install_dir:*) ;; *) export PATH=$quoted_install_dir:\"\${PATH-}\" ;; esac"
    if [ "${TREER_INSTALL_MODIFY_PATH:-1}" = 0 ]; then
        echo "treer: shell PATH configuration skipped"
    else
        case "${SHELL:-/bin/sh}" in
            */zsh)
                shell_directory=${ZDOTDIR:-"$HOME"}
                append_path "$shell_directory/.zprofile"
                append_path "$shell_directory/.zshrc"
                ;;
            */bash)
                append_path "$HOME/.bashrc"
                # Preserve bash's existing login-file precedence.
                if [ -f "$HOME/.bash_profile" ]; then
                    append_path "$HOME/.bash_profile"
                elif [ -f "$HOME/.bash_login" ]; then
                    append_path "$HOME/.bash_login"
                elif [ -f "$HOME/.profile" ]; then
                    append_path "$HOME/.profile"
                else
                    append_path "$HOME/.bash_profile"
                fi
                ;;
            */sh|*/dash|*/ksh) append_path "$HOME/.profile" ;;
            *) echo "treer: configure PATH manually for ${SHELL:-your shell}" ;;
        esac
    fi
    echo "treer: open a new terminal, or run this in the current shell:"
    printf 'export PATH=%s:"$PATH"\n' "$quoted_install_dir"
}

append_path() {
    profile=$1
    if [ -f "$profile" ] && grep -Fqx -- "$path_line" "$profile"; then
        return
    fi
    if mkdir -p "$(dirname -- "$profile")" && \
        printf '\n# Treer CLI and Controller\n%s\n' "$path_line" >> "$profile"; then
        echo "treer: configured PATH in $profile"
    else
        echo "treer: could not configure PATH in $profile; use the command below" >&2
    fi
}

configure_shell_path

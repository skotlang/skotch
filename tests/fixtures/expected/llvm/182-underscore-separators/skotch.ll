; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 1000000
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t0)
  %t2 = add i32 0, 65535
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t2)
  %t4 = add i32 0, 240
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  ret i32 0
}


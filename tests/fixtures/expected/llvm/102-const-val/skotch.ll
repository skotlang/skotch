; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [7 x i8] c"skotch\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = fadd double 0.0, 3.14159e0
  call i32 @puts(ptr %t0)
  call i32 @puts(ptr @.str.0)
  %t3 = add i32 0, 5
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t3)
  ret i32 0
}


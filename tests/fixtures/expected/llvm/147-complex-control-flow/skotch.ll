; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"collatz(\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c") = \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [18 x i8] c"collatz(%d) = %d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_collatz(i32 %arg0) {
entry:
  %merge_1 = alloca i32
  %merge_3 = alloca i32
  store i32 %arg0, ptr %merge_1
  %t0 = add i32 0, 0
  store i32 %t0, ptr %merge_3
  br label %bb1
bb1:
  %t1 = load i32, ptr %merge_1
  %t2 = add i32 0, 1
  %t3 = icmp ne i32 %t1, %t2
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_1
  %t5 = add i32 0, 2
  %t6 = srem i32 %t4, %t5
  %t7 = add i32 0, 0
  %t8 = icmp eq i32 %t6, %t7
  br i1 %t8, label %bb4, label %bb5
bb3:
  %t9 = load i32, ptr %merge_3
  ret i32 %t9
bb4:
  %t10 = load i32, ptr %merge_1
  %t11 = add i32 0, 2
  %t12 = sdiv i32 %t10, %t11
  store i32 %t12, ptr %merge_1
  br label %bb6
bb5:
  %t13 = load i32, ptr %merge_1
  %t14 = add i32 0, 3
  %t15 = mul i32 %t13, %t14
  %t16 = add i32 0, 1
  %t17 = add i32 %t15, %t16
  store i32 %t17, ptr %merge_1
  br label %bb6
bb6:
  %t18 = load i32, ptr %merge_3
  %t19 = add i32 0, 1
  %t20 = add i32 %t18, %t19
  store i32 %t20, ptr %merge_3
  br label %bb1
}

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 1
  %t1 = add i32 0, 10
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb4
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = call i32 @InputKt_collatz(i32 %t4)
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, i32 null, i32 %t5)
  br label %bb3
bb3:
  %t7 = add i32 0, 1
  %t8 = load i32, ptr %merge_2
  %t9 = add i32 %t8, %t7
  store i32 %t9, ptr %merge_2
  br label %bb1
bb4:
  ret i32 0
}

